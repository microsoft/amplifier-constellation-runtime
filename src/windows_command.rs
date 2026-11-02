//! Native Windows commands. AppContainer is the access boundary; a job owns
//! the whole process tree. Nothing here falls back to an ordinary process.
use crate::config::Config;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    ffi::{OsStr, OsString, c_void},
    fs::{File, OpenOptions},
    mem::{size_of, zeroed},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::io::AsyncReadExt;
use windows_sys::Win32::{
    Foundation::*,
    Security::{Authorization::*, Isolation::*, *},
    Storage::FileSystem::*,
    System::{
        JobObjects::*, Pipes::CreatePipe, SystemInformation::*, SystemServices::*, Threading::*,
    },
};

const MAX_ENTRIES: usize = 50_000;
struct PreparationControl {
    cancelled: AtomicBool,
    start: Mutex<()>,
    deadline: Instant,
    #[cfg(test)]
    before_resume: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
}
impl PreparationControl {
    fn new(seconds: u64) -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            start: Mutex::new(()),
            deadline: Instant::now() + Duration::from_secs(seconds),
            #[cfg(test)]
            before_resume: Mutex::new(None),
        }
    }
    fn checkpoint(&self) -> Result<()> {
        ensure!(
            !self.cancelled.load(Ordering::Acquire),
            "Command cancelled during preparation"
        );
        ensure!(
            Instant::now() < self.deadline,
            "Command preparation exceeded its deadline"
        );
        Ok(())
    }
}
struct CancelPreparation(Arc<PreparationControl>);
impl Drop for CancelPreparation {
    fn drop(&mut self) {
        // ResumeThread and cancellation share this lock. After cancellation
        // returns, a queued blocking worker cannot subsequently start a child.
        let _start = self.0.start.lock().unwrap_or_else(|e| e.into_inner());
        self.0.cancelled.store(true, Ordering::Release);
    }
}
fn wide(value: &OsStr) -> Result<Vec<u16>> {
    let mut value: Vec<u16> = value.encode_wide().collect();
    ensure!(
        !value.contains(&0),
        "A Windows path or argument contains a NUL"
    );
    value.push(0);
    Ok(value)
}
fn bool_ok(result: i32, operation: &str) -> Result<()> {
    if result == 0 {
        return Err(std::io::Error::last_os_error()).context(operation.to_owned());
    }
    Ok(())
}
fn win_ok(result: u32, operation: &str) -> Result<()> {
    if result != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(result as i32)).context(operation.to_owned());
    }
    Ok(())
}
fn owned(handle: HANDLE, operation: &str) -> Result<OwnedHandle> {
    ensure!(
        !handle.is_null() && handle != INVALID_HANDLE_VALUE,
        "{operation}: {}",
        std::io::Error::last_os_error()
    );
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}
fn raw(handle: &OwnedHandle) -> HANDLE {
    handle.as_raw_handle()
}
struct Local(*mut c_void);
impl Drop for Local {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}

struct Profile {
    name: Vec<u16>,
    sid: Box<[u32]>,
}
impl Profile {
    fn new() -> Result<Self> {
        let name = wide(OsStr::new(&format!(
            "AgentRuntime.{}",
            uuid::Uuid::new_v4().simple()
        )))?;
        let mut sid = null_mut();
        let result = unsafe {
            CreateAppContainerProfile(
                name.as_ptr(),
                name.as_ptr(),
                name.as_ptr(),
                null(),
                0,
                &mut sid,
            )
        };
        ensure!(
            result >= 0,
            "Creating the Windows AppContainer failed (HRESULT {result:#x}); commands remain disabled"
        );
        let bytes = unsafe { GetLengthSid(sid) } as usize;
        let mut copy = vec![0u32; bytes.div_ceil(4)].into_boxed_slice();
        unsafe {
            std::ptr::copy_nonoverlapping(sid.cast::<u8>(), copy.as_mut_ptr().cast(), bytes);
            FreeSid(sid);
        }
        Ok(Self { name, sid: copy })
    }
    fn sid(&self) -> PSID {
        self.sid.as_ptr().cast_mut().cast()
    }
}
impl Drop for Profile {
    fn drop(&mut self) {
        unsafe {
            DeleteAppContainerProfile(self.name.as_ptr());
        }
    }
}

/// Exclusive ACL-update handles stop SetSecurityInfo from propagating through
/// unchecked children. We open/check every existing object individually. The
/// separate read-only pin allows ordinary file use while preventing root moves.
fn open_checked(path: &Path, boundary: &Path, pin: bool) -> Result<(OwnedHandle, bool)> {
    let path_w = wide(path.as_os_str())?;
    let file = owned(
        unsafe {
            CreateFileW(
                path_w.as_ptr(),
                if pin {
                    FILE_READ_ATTRIBUTES | READ_CONTROL
                } else {
                    FILE_READ_ATTRIBUTES | READ_CONTROL | WRITE_DAC
                },
                if pin {
                    FILE_SHARE_READ | FILE_SHARE_WRITE
                } else {
                    0
                },
                null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                null_mut(),
            )
        },
        "Open command workspace object",
    )?;
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { zeroed() };
    bool_ok(
        unsafe { GetFileInformationByHandle(raw(&file), &mut info) },
        "Inspect command workspace object",
    )?;
    ensure!(
        info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT == 0,
        "Commands refuse workspace reparse points: {}",
        path.display()
    );
    let directory = info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    ensure!(
        directory || info.nNumberOfLinks == 1,
        "Commands refuse multiply linked workspace files: {}",
        path.display()
    );
    let mut name = vec![0u16; 32768];
    let length =
        unsafe { GetFinalPathNameByHandleW(raw(&file), name.as_mut_ptr(), name.len() as u32, 0) }
            as usize;
    ensure!(
        length > 0 && length < name.len(),
        "Cannot verify final command workspace path"
    );
    let final_path = PathBuf::from(OsString::from_wide(&name[..length]));
    ensure!(
        final_path.starts_with(boundary),
        "Workspace object resolved outside its boundary"
    );
    Ok((file, directory))
}

fn acl_change(file: &OwnedHandle, sid: PSID, directory: bool, grant: bool) -> Result<()> {
    let mut descriptor = null_mut();
    let mut old_acl: *mut ACL = null_mut();
    win_ok(
        unsafe {
            GetSecurityInfo(
                raw(file),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                &mut old_acl,
                null_mut(),
                &mut descriptor,
            )
        },
        "Read workspace permissions",
    )?;
    let _descriptor = Local(descriptor);
    ensure!(
        !old_acl.is_null(),
        "Commands refuse a workspace object without an explicit DACL"
    );
    let mut control = 0;
    let mut revision = 0;
    bool_ok(
        unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) },
        "Read workspace inheritance policy",
    )?;
    let protected = control & SE_DACL_PROTECTED != 0;
    let mut filtered = vec![0u32; unsafe { (*old_acl).AclSize as usize }.div_ceil(4) + 8];
    let explicit: *mut ACL = filtered.as_mut_ptr().cast();
    bool_ok(
        unsafe {
            InitializeAcl(
                explicit,
                (filtered.len() * 4) as u32,
                (*old_acl).AclRevision as u32,
            )
        },
        "Prepare workspace permission update",
    )?;
    for index in 0..unsafe { (*old_acl).AceCount as u32 } {
        let mut ace = null_mut();
        bool_ok(
            unsafe { GetAce(old_acl, index, &mut ace) },
            "Read workspace permission entry",
        )?;
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        let ours = header.AceType as u32 == ACCESS_ALLOWED_ACE_TYPE
            && unsafe {
                EqualSid(
                    std::ptr::addr_of!((*ace.cast::<ACCESS_ALLOWED_ACE>()).SidStart)
                        .cast_mut()
                        .cast(),
                    sid,
                ) != 0
            };
        // An unprotected DACL must be supplied as explicit entries. Windows
        // recomputes inheritance from its current parent; copying inherited
        // entries into that input duplicates them as explicit permissions.
        if (!ours || grant) && (protected || header.AceFlags as u32 & INHERITED_ACE == 0) {
            bool_ok(
                unsafe {
                    AddAce(
                        explicit,
                        (*old_acl).AclRevision as u32,
                        u32::MAX,
                        ace,
                        header.AceSize as u32,
                    )
                },
                "Preserve workspace permission entry",
            )?;
        }
    }
    let mut next = explicit;
    let allocation = if grant {
        let entry = EXPLICIT_ACCESS_W {
            grfAccessPermissions: FILE_GENERIC_READ
                | FILE_GENERIC_WRITE
                | FILE_GENERIC_EXECUTE
                | DELETE,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: if directory {
                OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
            } else {
                0
            },
            Trustee: TRUSTEE_W {
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: sid.cast(),
                ..Default::default()
            },
        };
        win_ok(
            unsafe { SetEntriesInAclW(1, &entry, explicit, &mut next) },
            "Grant AppContainer workspace access",
        )?;
        Some(Local(next.cast()))
    } else {
        None
    };
    win_ok(
        unsafe {
            SetSecurityInfo(
                raw(file),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION
                    | if protected {
                        PROTECTED_DACL_SECURITY_INFORMATION
                    } else {
                        UNPROTECTED_DACL_SECURITY_INFORMATION
                    },
                null_mut(),
                null_mut(),
                next,
                null(),
            )
        },
        "Update AppContainer workspace access",
    )?;
    drop(allocation);
    Ok(())
}

fn paths(root: &Path, control: &PreparationControl) -> Result<Vec<PathBuf>> {
    let mut result = vec![root.to_path_buf()];
    let mut index = 0;
    while index < result.len() {
        control.checkpoint()?;
        let meta = std::fs::symlink_metadata(&result[index])?;
        use std::os::windows::fs::MetadataExt;
        ensure!(
            meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
            "Commands refuse workspace reparse points"
        );
        if meta.is_dir() {
            for entry in std::fs::read_dir(&result[index])? {
                control.checkpoint()?;
                ensure!(
                    result.len() < MAX_ENTRIES,
                    "Workspace exceeds Windows command preparation budget"
                );
                result.push(entry?.path());
            }
        }
        index += 1;
    }
    Ok(result)
}

fn cleanup_paths(root: &Path) -> (Vec<PathBuf>, usize) {
    use std::os::windows::fs::MetadataExt;
    let mut queue = vec![root.to_path_buf()];
    let mut safe = Vec::new();
    let mut issues = 0;
    let mut index = 0;
    while index < queue.len() {
        let path = &queue[index];
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 => {
                safe.push(path.clone());
                if meta.is_dir() {
                    match std::fs::read_dir(path) {
                        Ok(entries) => {
                            for entry in entries {
                                if queue.len() >= MAX_ENTRIES {
                                    return (safe, issues + 1);
                                }
                                match entry {
                                    Ok(entry) => queue.push(entry.path()),
                                    Err(_) => issues += 1,
                                }
                            }
                        }
                        Err(_) => issues += 1,
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            _ => issues += 1,
        }
        index += 1;
    }
    (safe, issues)
}

struct Execution {
    job: OwnedHandle,
    process: Option<OwnedHandle>,
    roots: Vec<PathBuf>,
    granted: Vec<PathBuf>,
    profile: Profile,
    _pins: Vec<OwnedHandle>,
    _scratch: tempfile::TempDir,
    _locks: Vec<File>,
    cleaned: bool,
}
impl Execution {
    fn cleanup(&mut self) -> usize {
        if self.cleaned {
            return 0;
        }
        self.cleaned = true;
        let mut issues = 0;
        unsafe {
            TerminateJobObject(raw(&self.job), 1);
            if let Some(process) = &self.process {
                WaitForSingleObject(raw(process), 3000);
            }
        }
        // These read-only handles prevent root renames while a process runs.
        // Release them before opening exclusive handles for ACL cleanup.
        self._pins.clear();
        // Existing objects and any new descendants lose only this run's SID.
        // A reparse point is never followed during cleanup. Failed cleanup
        // leaves an inert unique SID after the AppContainer profile is deleted.
        for root in &self.roots {
            let (current, skipped) = cleanup_paths(root);
            self.granted.extend(current);
            issues += skipped;
        }
        self.granted.sort();
        self.granted.dedup();
        for path in &self.granted {
            if std::fs::symlink_metadata(path)
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                continue;
            }
            if let Some(root) = self.roots.iter().find(|root| path.starts_with(root)) {
                match open_checked(path, root, false) {
                    Ok((file, directory)) => {
                        if acl_change(&file, self.profile.sid(), directory, false).is_err() {
                            issues += 1;
                        }
                    }
                    Err(_) => issues += 1,
                }
            }
        }
        if issues > 0 {
            eprintln!(
                "Windows command cleanup could not verify {issues} workspace entries; its temporary AppContainer identity is being removed"
            );
        }
        issues
    }
}
impl Drop for Execution {
    fn drop(&mut self) {
        self.cleanup();
    }
}

struct Attributes {
    storage: Vec<usize>,
    initialized: bool,
}
impl Attributes {
    fn new(count: u32) -> Result<Self> {
        let mut bytes = 0;
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), count, 0, &mut bytes);
        }
        ensure!(
            bytes > 0 && bytes < 65536,
            "Invalid Windows startup attribute size"
        );
        let mut value = Self {
            storage: vec![0; bytes.div_ceil(size_of::<usize>())],
            initialized: false,
        };
        bool_ok(
            unsafe { InitializeProcThreadAttributeList(value.ptr(), count, 0, &mut bytes) },
            "Initialize isolated command startup",
        )?;
        value.initialized = true;
        Ok(value)
    }
    fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr().cast()
    }
    fn set<T>(&mut self, key: usize, value: &T) -> Result<()> {
        bool_ok(
            unsafe {
                UpdateProcThreadAttribute(
                    self.ptr(),
                    0,
                    key,
                    (value as *const T).cast(),
                    size_of::<T>(),
                    null_mut(),
                    null(),
                )
            },
            "Set isolated command startup attribute",
        )
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        if self.initialized {
            unsafe {
                DeleteProcThreadAttributeList(self.ptr());
            }
        }
    }
}

fn pipes() -> Result<(OwnedHandle, OwnedHandle)> {
    let mut read = null_mut();
    let mut write = null_mut();
    let attr = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };
    bool_ok(
        unsafe { CreatePipe(&mut read, &mut write, &attr, 0) },
        "Create command output pipe",
    )?;
    Ok((owned(read, "Pipe read")?, owned(write, "Pipe write")?))
}
fn process_path(path: &Path) -> Result<PathBuf> {
    let value = path
        .to_str()
        .context("Windows process paths must be Unicode")?;
    let value = value.strip_prefix("\\\\?\\").unwrap_or(value);
    ensure!(
        value.len() < 240 && value.as_bytes().get(1) == Some(&b':'),
        "Windows commands currently require ordinary local paths shorter than 240 characters"
    );
    Ok(PathBuf::from(value))
}

fn system_directory() -> Result<PathBuf> {
    let mut buffer = vec![0u16; 32768];
    let n = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    ensure!(
        n > 0 && n < buffer.len(),
        "Cannot locate the Windows system directory"
    );
    Ok(PathBuf::from(OsString::from_wide(&buffer[..n])))
}
fn argument(value: &OsStr) -> Result<Vec<u16>> {
    let value = wide(value)?;
    let mut out = vec![b'"' as u16];
    let mut slashes = 0;
    for &unit in &value[..value.len() - 1] {
        if unit == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        if unit == b'"' as u16 {
            out.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2 + 1));
        } else {
            out.extend(std::iter::repeat_n(b'\\' as u16, slashes));
        }
        out.push(unit);
        slashes = 0;
    }
    out.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    out.push(b'"' as u16);
    Ok(out)
}

async fn capture(mut file: tokio::fs::File, limit: usize) -> Result<(String, bool)> {
    let mut output = Vec::new();
    let mut truncated = false;
    let mut bytes = [0; 4096];
    loop {
        let count = file.read(&mut bytes).await?;
        if count == 0 {
            break;
        }
        let take = count.min(limit - output.len());
        output.extend_from_slice(&bytes[..take]);
        truncated |= take < count;
    }
    Ok((String::from_utf8_lossy(&output).into_owned(), truncated))
}

fn prepare(
    c: &Config,
    cwd: &Path,
    args: &Value,
    control: &PreparationControl,
) -> Result<(Execution, File, File)> {
    control.checkpoint()?;
    ensure!(
        cfg!(feature = "windows-appcontainer"),
        "Windows commands are disabled in this build; no unrestricted fallback"
    );
    let name = args["command"].as_str().context("Missing command")?;
    let arguments: Vec<String> = serde_json::from_value(args["args"].clone())?;
    ensure!(
        !name.is_empty()
            && name.len() <= 4096
            && arguments.len() <= 128
            && arguments.iter().all(|s| s.len() <= 16384),
        "Command exceeds bounds"
    );
    let mut roots = c.effective_roots()?;
    let all_roots = roots.clone();
    roots.retain(|root| {
        !all_roots
            .iter()
            .any(|other| root != other && root.starts_with(other))
    });
    roots.sort();
    let cwd = std::fs::canonicalize(cwd)?;
    ensure!(
        roots.iter().any(|root| cwd.starts_with(root)),
        "Command folder is outside workspace"
    );
    // A local NTFS workspace is required; no network drives or device paths.
    for root in &roots {
        let display = root.to_string_lossy();
        ensure!(
            display.starts_with("\\\\?\\") && display.as_bytes().get(5) == Some(&b':'),
            "Windows commands require local drive workspaces"
        );
    }
    let system = std::fs::canonicalize(system_directory()?)?;
    let chosen = Path::new(name);
    let command = std::fs::canonicalize(if chosen.is_absolute() {
        chosen.to_owned()
    } else if chosen.components().count() == 1 {
        system.join(if chosen.extension().is_none() {
            format!("{name}.exe")
        } else {
            name.to_owned()
        })
    } else {
        cwd.join(chosen)
    })?;
    ensure!(
        roots.iter().any(|root| command.starts_with(root)) || command.starts_with(&system),
        "Command executable must be inside the workspace or Windows system directory"
    );
    ensure!(
        command
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("exe")),
        "Invoke an executable explicitly; use cmd.exe for batch commands"
    );

    let locks = std::env::temp_dir().join("agent-runtime-command-locks");
    crate::config::private_dir(&locks)?;
    let mut held_locks = Vec::new();
    let mut entries = Vec::new();
    for root in &roots {
        let lock_path = locks.join(format!(
            "{:x}.lock",
            Sha256::digest(root.to_string_lossy().as_bytes())
        ));
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        fs2::FileExt::try_lock_exclusive(&lock)
            .context("Another command is using this workspace; retry when it completes")?;
        held_locks.push(lock);
        entries.extend(paths(root, control)?);
        ensure!(
            entries.len() <= MAX_ENTRIES,
            "Granted workspaces exceed the command entry budget"
        );
    }
    // Validate every path before granting any access or starting a process.
    for entry in &entries {
        control.checkpoint()?;
        let root = roots
            .iter()
            .find(|root| entry.starts_with(root))
            .context("Ungrounded workspace entry")?;
        open_checked(entry, root, true)?;
    }
    let profile = Profile::new()?;
    let scratch = tempfile::Builder::new()
        .prefix("agent-command-")
        .tempdir()?;
    let scratch_path = std::fs::canonicalize(scratch.path())?;
    let job = owned(
        unsafe { CreateJobObjectW(null(), null()) },
        "Create command job",
    )?;
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    limits.BasicLimitInformation.LimitFlags =
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
    limits.BasicLimitInformation.ActiveProcessLimit = 64;
    bool_ok(
        unsafe {
            SetInformationJobObject(
                raw(&job),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        },
        "Constrain command job",
    )?;
    let mut execution = Execution {
        job,
        process: None,
        roots: roots
            .iter()
            .cloned()
            .chain(std::iter::once(scratch_path.clone()))
            .collect(),
        granted: Vec::new(),
        profile,
        _pins: Vec::new(),
        _scratch: scratch,
        _locks: held_locks,
        cleaned: false,
    };
    for entry in entries.iter().chain(std::iter::once(&scratch_path)) {
        control.checkpoint()?;
        let root = roots
            .iter()
            .find(|root| entry.starts_with(root))
            .unwrap_or(&scratch_path);
        let (file, directory) = open_checked(entry, root, false)?;
        acl_change(&file, execution.profile.sid(), directory, true)?;
        execution.granted.push(entry.clone());
    }
    execution._pins = execution
        .roots
        .iter()
        .map(|root| open_checked(root, root, true).map(|entry| entry.0))
        .collect::<Result<Vec<_>>>()?;
    let (stdin_read, stdin_write) = pipes()?;
    let (stdout_read, stdout_write) = pipes()?;
    let (stderr_read, stderr_write) = pipes()?;
    for parent in [&stdout_read, &stderr_read, &stdin_write] {
        bool_ok(
            unsafe { SetHandleInformation(raw(parent), HANDLE_FLAG_INHERIT, 0) },
            "Isolate parent command handles",
        )?;
    }
    drop(stdin_write); // EOF, with no access to a host console or device.
    let handles = [raw(&stdin_read), raw(&stdout_write), raw(&stderr_write)];
    let capabilities = SECURITY_CAPABILITIES {
        AppContainerSid: execution.profile.sid(),
        Capabilities: null_mut(),
        CapabilityCount: 0,
        Reserved: 0,
    };
    let mut attributes = Attributes::new(2)?;
    attributes.set(
        PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
        &capabilities,
    )?;
    attributes.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize, &handles)?;
    let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = handles[0];
    startup.StartupInfo.hStdOutput = handles[1];
    startup.StartupInfo.hStdError = handles[2];
    startup.lpAttributeList = attributes.ptr();
    let process_command = process_path(&command)?;
    let process_cwd = process_path(&cwd)?;
    let process_scratch = process_path(&scratch_path)?;
    let process_system = process_path(&system)?;
    let mut line = argument(process_command.as_os_str())?;
    if command.starts_with(&system)
        && command
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("cmd.exe"))
    {
        ensure!(
            arguments.len() >= 2
                && arguments[arguments.len() - 2].eq_ignore_ascii_case("/c")
                && arguments[..arguments.len() - 2]
                    .iter()
                    .all(|a| a.eq_ignore_ascii_case("/d") || a.eq_ignore_ascii_case("/s")),
            "cmd.exe requires /c followed by one command string; only optional /d and /s switches are supported"
        );
        // cmd.exe does not use the CRT argv parser. /s strips exactly the outer
        // command-string quotes; /d prevents AutoRun registry commands.
        let shell = wide(OsStr::new(&format!(
            " /d /s /c \"{}\"",
            arguments.last().unwrap()
        )))?;
        line.extend_from_slice(&shell[..shell.len() - 1]);
    } else {
        for arg in &arguments {
            line.push(b' ' as u16);
            line.extend(argument(OsStr::new(arg))?);
        }
    }
    ensure!(line.len() < 32766, "Windows command line exceeds its bound");
    line.push(0);
    let windows = process_system
        .parent()
        .context("Cannot locate Windows folder")?;
    let values = [
        ("APPDATA", process_scratch.clone()),
        ("COMSPEC", process_system.join("cmd.exe")),
        ("HOME", process_scratch.clone()),
        ("LOCALAPPDATA", process_scratch.clone()),
        ("PATH", process_system.clone()),
        ("SystemRoot", windows.to_owned()),
        ("TEMP", process_scratch.clone()),
        ("TMP", process_scratch.clone()),
        ("USERPROFILE", process_scratch.clone()),
        ("WINDIR", windows.to_owned()),
    ];
    let mut environment = Vec::new();
    for (key, value) in values {
        let mut entry = OsString::from(key);
        entry.push("=");
        entry.push(value);
        environment.extend(wide(&entry)?);
    }
    environment.push(0);
    let executable = wide(process_command.as_os_str())?;
    let cwd_wide = wide(process_cwd.as_os_str())?;
    let mut process: PROCESS_INFORMATION = unsafe { zeroed() };
    control.checkpoint()?;
    bool_ok(
        unsafe {
            CreateProcessW(
                executable.as_ptr(),
                line.as_mut_ptr(),
                null(),
                null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT
                    | CREATE_UNICODE_ENVIRONMENT
                    | CREATE_SUSPENDED
                    | CREATE_NO_WINDOW
                    | CREATE_DEFAULT_ERROR_MODE,
                environment.as_ptr().cast(),
                cwd_wide.as_ptr(),
                &startup.StartupInfo,
                &mut process,
            )
        },
        "Start AppContainer command",
    )?;
    let child = owned(process.hProcess, "Command process")?;
    let thread = owned(process.hThread, "Command thread")?;
    // If job assignment fails, a still-suspended process must never escape.
    if unsafe { AssignProcessToJobObject(raw(&execution.job), raw(&child)) } == 0 {
        let error = std::io::Error::last_os_error();
        unsafe {
            TerminateProcess(raw(&child), 1);
        }
        return Err(error).context("Assign isolated command to its job");
    }
    execution.process = Some(child);
    #[cfg(test)]
    if let Some((ready, release)) = control.before_resume.lock().unwrap().take() {
        ready.send(()).unwrap();
        release.recv().unwrap();
    }
    let _start = control.start.lock().unwrap_or_else(|e| e.into_inner());
    control.checkpoint()?;
    ensure!(
        unsafe { ResumeThread(raw(&thread)) } != u32::MAX,
        "Could not start the confined command thread"
    );
    drop(thread);
    drop(stdin_read);
    drop(stdout_write);
    drop(stderr_write);
    drop(attributes);
    Ok((execution, File::from(stdout_read), File::from(stderr_read)))
}

pub async fn run(c: &Config, cwd: &Path, args: &Value) -> Result<Value> {
    // Blocking setup has its own cancellation guard, including a final locked
    // start decision. Merely dropping a spawn_blocking handle would be unsafe.
    let control = Arc::new(PreparationControl::new(c.limits.command_seconds));
    let _cancel = CancelPreparation(control.clone());
    let deadline = tokio::time::Instant::from_std(control.deadline);
    let (config, cwd, args) = (c.clone(), cwd.to_owned(), args.clone());
    let preparation = tokio::task::spawn_blocking(move || prepare(&config, &cwd, &args, &control));
    let (mut execution, stdout, stderr) = tokio::time::timeout_at(deadline, preparation)
        .await
        .context("Command preparation timed out")?
        .context("Command preparation failed")??;
    let stdout = tokio::fs::File::from_std(stdout);
    let stderr = tokio::fs::File::from_std(stderr);
    let wait = async {
        loop {
            let status =
                unsafe { WaitForSingleObject(raw(execution.process.as_ref().unwrap()), 0) };
            if status == WAIT_OBJECT_0 {
                let mut code = 0;
                bool_ok(
                    unsafe {
                        GetExitCodeProcess(raw(execution.process.as_ref().unwrap()), &mut code)
                    },
                    "Read command exit status",
                )?;
                // Descendants may still hold output pipes after the parent exits.
                bool_ok(
                    unsafe { TerminateJobObject(raw(&execution.job), 1) },
                    "Finish command descendants",
                )?;
                return Ok::<_, anyhow::Error>(code);
            }
            ensure!(status == WAIT_TIMEOUT, "Failed to observe command status");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    let (code, out, err) = tokio::time::timeout_at(deadline, async {
        tokio::try_join!(
            wait,
            capture(stdout, c.limits.tool_output_bytes),
            capture(stderr, c.limits.tool_output_bytes)
        )
    })
    .await
    .context("Command timed out; inspect workspace effects before retrying")??;
    let cleanup_incomplete = execution.cleanup();
    drop(execution);
    Ok(
        json!({"exit_code":code,"success":code==0,"stdout":out.0,"stderr":err.0,"truncated":out.1||err.1,"cleanup_incomplete":cleanup_incomplete}),
    )
}

#[cfg(all(test, feature = "windows-appcontainer"))]
mod tests {
    use super::*;
    #[test]
    fn cancellation_before_resume_never_executes_the_command() {
        preparation_boundary(false);
    }
    #[test]
    fn deadline_before_resume_never_executes_the_command() {
        preparation_boundary(true);
    }
    fn preparation_boundary(expire: bool) {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let config = Config::initial(
            workspace.clone(),
            "Preparation test".into(),
            "fixture".into(),
        );
        let control = Arc::new(PreparationControl::new(if expire { 1 } else { 10 }));
        let cancel = CancelPreparation(control.clone());
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *control.before_resume.lock().unwrap() = Some((ready_tx, release_rx));
        let worker = std::thread::spawn(move || {
            prepare(
                &config,
                &config.workspace,
                &json!({"command":"cmd.exe","args":["/d","/c","echo unexpected>escaped"]}),
                &control,
            )
        });
        ready_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("Preparation must reach suspended-process barrier");
        if expire {
            std::thread::sleep(Duration::from_millis(1100));
        } else {
            drop(cancel);
        }
        release_tx.send(()).unwrap();
        let failure = match worker.join().unwrap() {
            Err(error) => error,
            Ok(_) => panic!("Command resumed after its request ended"),
        };
        assert!(
            format!("{failure:#}").contains(if expire { "deadline" } else { "cancelled" }),
            "{failure:#}"
        );
        assert!(!workspace.join("escaped").exists());
    }
}

#![cfg(all(windows, feature = "windows-appcontainer"))]
use agent_runtime::{config::Config, tools};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

/// This same native test executable is copied into the granted workspace. It
/// performs actual OS operations, not mocked access checks or shell emulation.
#[test]
fn command_child_fixture() {
    if std::env::current_exe().unwrap().file_name().unwrap() != "command-probe.exe" {
        return;
    }
    let data = std::fs::read("command-probe.json")
        .expect("Confined child must be able to read the workspace fixture");
    let data: Value = serde_json::from_slice(&data).unwrap();
    match data["operation"].as_str().unwrap() {
        "multiple_roots" => {
            let shared = data["shared"].as_str().unwrap();
            if data["allowed"].as_bool().unwrap() {
                assert_eq!(std::fs::read_to_string(shared).unwrap(), "shared-visible");
                std::fs::write(shared, "shared-updated").unwrap();
            } else {
                assert!(std::fs::read(shared).is_err());
                assert!(std::fs::write(shared, "escaped").is_err());
            }
            assert!(std::fs::read(data["outside"].as_str().unwrap()).is_err());
            println!("MULTIPLE_ROOTS_PASSED");
        }
        "access" => {
            use windows_sys::Win32::{Foundation::*, Security::*, System::Threading::*};
            let mut token = std::ptr::null_mut();
            let mut value = 0u32;
            let mut length = 0;
            unsafe {
                assert_ne!(
                    OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token),
                    0
                );
                assert_ne!(
                    GetTokenInformation(
                        token,
                        TokenIsAppContainer,
                        (&mut value as *mut u32).cast(),
                        4,
                        &mut length
                    ),
                    0
                );
                CloseHandle(token);
            }
            assert_eq!(value, 1, "Child is not an AppContainer");
            std::fs::create_dir_all("nested/new").unwrap();
            std::fs::write("nested/new/note", "inside").unwrap();
            assert_eq!(
                std::fs::read_to_string("nested/new/note").unwrap(),
                "inside"
            );
            std::fs::rename("nested/new/note", "nested/new/renamed").unwrap();
            for key in ["outside", "runtime_secret", "user_secret"] {
                let path = data[key].as_str().unwrap();
                assert!(
                    std::fs::read(path).is_err(),
                    "Outside read succeeded: {key}"
                );
                assert!(
                    std::fs::write(path, "escaped").is_err(),
                    "Outside write succeeded: {key}"
                );
                assert!(
                    std::fs::hard_link(path, format!("hard-link-{key}")).is_err(),
                    "Outside hard link succeeded"
                );
            }
            let address = data["socket"].as_str().unwrap().parse().unwrap();
            assert!(
                std::net::TcpStream::connect_timeout(&address, Duration::from_secs(2)).is_err(),
                "Loopback connection escaped"
            );
            assert!(!std::env::vars().any(|(k, _)| k.contains("TOKEN")
                || k.contains("API_KEY")
                || k == "SSH_AUTH_SOCK"
                || k == "COMMAND_TEST_SECRET"));
            println!("ACTUAL_APPCONTAINER_ACCESS_PASSED");
        }
        "wait" => {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "descendant_child_fixture", "--nocapture"])
                .spawn()
                .unwrap();
            std::fs::write("started", "yes").unwrap();
            let _ = child.wait();
        }
        "output" => {
            println!("{}", "x".repeat(500_000));
            eprintln!("{}", "y".repeat(500_000));
        }
        "cleanup" => {
            std::fs::write("new-file", "preserve this file").unwrap();
            std::fs::write("cleanup-ready", "yes").unwrap();
            while !Path::new("cleanup-release").exists() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        _ => panic!("Unknown test operation"),
    }
}

#[test]
fn descendant_child_fixture() {
    if !Path::new("command-probe.json").exists() {
        return;
    }
    std::fs::write("descendant-started", "yes").unwrap();
    while !Path::new("release").exists() {
        std::thread::sleep(Duration::from_millis(10));
    }
    std::fs::write("escaped", "bad").unwrap();
}

fn setup() -> (tempfile::TempDir, Config, Value) {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace with spaces");
    std::fs::create_dir(&workspace).unwrap();
    let binary = workspace.join("command-probe.exe");
    std::fs::copy(std::env::current_exe().unwrap(), &binary).unwrap();
    let config = Config::initial(workspace, "Windows command test".into(), "fixture".into());
    (
        temp,
        config,
        json!({"command":binary,"args":["--exact","command_child_fixture","--nocapture"]}),
    )
}
fn acl_entries(path: &Path) -> Vec<Vec<u8>> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::{
        Foundation::*,
        Security::{Authorization::*, *},
        Storage::FileSystem::*,
    };
    let file = std::fs::OpenOptions::new()
        .access_mode(READ_CONTROL)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .unwrap();
    let mut acl = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    let mut entries = vec![];
    unsafe {
        assert_eq!(
            GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut acl,
                std::ptr::null_mut(),
                &mut descriptor
            ),
            ERROR_SUCCESS
        );
        assert!(!acl.is_null());
        for i in 0..(*acl).AceCount as u32 {
            let mut ace = std::ptr::null_mut();
            assert_ne!(GetAce(acl, i, &mut ace), 0);
            let size = (*ace.cast::<ACE_HEADER>()).AceSize as usize;
            entries.push(std::slice::from_raw_parts(ace.cast::<u8>(), size).to_vec());
        }
        LocalFree(descriptor);
    }
    entries
}
fn has_appcontainer_entry(path: &Path) -> bool {
    acl_entries(path).iter().any(|ace| {
        ace.len() >= 20
            && ace[0] == 0
            && ace[8] == 1
            && ace[15] == 15
            && u32::from_le_bytes(ace[16..20].try_into().unwrap()) == 2
    })
}
async fn wait_file(path: &Path) {
    for _ in 0..1000 {
        if path.is_file() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("Child did not reach expected point: {}", path.display());
}

#[tokio::test]
async fn actual_windows_access_network_environment_and_file_effects() {
    assert_eq!(tools::command_isolation(), Some("windows-appcontainer"));
    let (temp, c, args) = setup();
    let outside = temp.path().join("outside");
    std::fs::write(&outside, "private").unwrap();
    let state = temp.path().join("runtime-secret");
    std::fs::write(&state, "credential").unwrap();
    let user = temp.path().join("user-home-secret");
    std::fs::write(&user, "personal").unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    std::fs::write(c.workspace.join("command-probe.json"),json!({"operation":"access","outside":outside,"runtime_secret":state,"user_secret":user,"socket":listener.local_addr().unwrap().to_string()}).to_string()).unwrap();
    assert!(
        std::env::var("COMMAND_TEST_SECRET").is_ok(),
        "CI must seed an inherited-secret sentinel"
    );
    let before_file = acl_entries(&c.workspace.join("command-probe.json"));
    let before_root = acl_entries(&c.workspace);
    let result = tools::call(&c, &c.workspace, "workspace_run", &args)
        .await
        .unwrap();
    assert_eq!(result["success"], true, "{result}");
    assert!(
        result["stdout"]
            .as_str()
            .unwrap()
            .contains("ACTUAL_APPCONTAINER_ACCESS_PASSED"),
        "{result}"
    );
    assert_eq!(
        std::fs::read_to_string(c.workspace.join("nested/new/renamed")).unwrap(),
        "inside"
    );
    assert_eq!(std::fs::read_to_string(outside).unwrap(), "private");
    assert!(listener.accept().is_err());
    assert_eq!(
        acl_entries(&c.workspace.join("command-probe.json")),
        before_file,
        "Existing file permissions changed"
    );
    assert_eq!(
        acl_entries(&c.workspace),
        before_root,
        "Workspace permissions changed"
    );
    std::fs::write(c.workspace.join("nested/new/renamed"), "host still writes").unwrap();
    for path in ["nested", "nested/new", "nested/new/renamed"] {
        assert!(
            !has_appcontainer_entry(&c.workspace.join(path)),
            "Temporary package permission remains on {path}"
        );
    }
    assert!(
        tools::call(&c, temp.path(), "workspace_run", &args)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn additional_roots_have_real_access_and_are_revoked_on_reconnect() {
    let (temp, mut config, args) = setup();
    let shared = temp.path().join("shared");
    std::fs::create_dir(&shared).unwrap();
    let shared_file = shared.join("data");
    std::fs::write(&shared_file, "shared-visible").unwrap();
    let outside = temp.path().join("outside-data");
    std::fs::write(&outside, "private").unwrap();
    let original_acl = acl_entries(&shared_file);
    config.additional_workspaces = vec![shared.clone()];
    let session = config
        .for_session(config.workspace.clone(), &[shared])
        .unwrap();
    std::fs::write(
        config.workspace.join("command-probe.json"),
        json!({"operation":"multiple_roots","shared":shared_file,"outside":outside,"allowed":true})
            .to_string(),
    )
    .unwrap();
    let result = tools::call(&session, &config.workspace, "workspace_run", &args)
        .await
        .unwrap();
    assert_eq!(result["exit_code"], 0, "{result}");
    assert!(
        result["stdout"]
            .as_str()
            .unwrap()
            .contains("MULTIPLE_ROOTS_PASSED")
    );
    assert_eq!(
        std::fs::read_to_string(&shared_file).unwrap(),
        "shared-updated"
    );
    assert_eq!(acl_entries(&shared_file), original_acl);
    let resumed = config.for_session(config.workspace.clone(), &[]).unwrap();
    std::fs::write(config.workspace.join("command-probe.json"), json!({"operation":"multiple_roots","shared":shared_file,"outside":outside,"allowed":false}).to_string()).unwrap();
    let result = tools::call(&resumed, &config.workspace, "workspace_run", &args)
        .await
        .unwrap();
    assert_eq!(result["exit_code"], 0, "{result}");
    assert_eq!(
        std::fs::read_to_string(&shared_file).unwrap(),
        "shared-updated"
    );
    assert_eq!(acl_entries(&shared_file), original_acl);
}

#[tokio::test]
async fn windows_startup_failure_restores_existing_permissions() {
    let (_temp, c, _) = setup();
    let bad = c.workspace.join("invalid.exe");
    std::fs::write(&bad, "not a Windows executable").unwrap();
    let before_file = acl_entries(&bad);
    let before_root = acl_entries(&c.workspace);
    let result = tools::call(
        &c,
        &c.workspace,
        "workspace_run",
        &json!({"command":bad,"args":[]}),
    )
    .await;
    let failure = result.unwrap_err();
    assert!(
        format!("{failure:#}").contains("Start AppContainer command"),
        "{failure:#}"
    );
    assert_eq!(acl_entries(&bad), before_file);
    assert_eq!(acl_entries(&c.workspace), before_root);
    std::fs::write(&bad, "owner access remains").unwrap();
}

#[tokio::test]
async fn windows_cleanup_skips_new_junction_and_cleans_safe_siblings() {
    let (temp, c, args) = setup();
    let external = temp.path().join("external");
    std::fs::create_dir(&external).unwrap();
    std::fs::write(external.join("secret"), "private").unwrap();
    let external_acl = acl_entries(&external);
    std::fs::write(
        c.workspace.join("command-probe.json"),
        json!({"operation":"cleanup","external_directory":external}).to_string(),
    )
    .unwrap();
    let running = c.clone();
    let task = tokio::spawn(async move {
        tools::call(&running, &running.workspace, "workspace_run", &args).await
    });
    wait_file(&c.workspace.join("cleanup-ready")).await;
    // AppContainer denies junction creation itself. A concurrent host editor
    // can still introduce one after preflight, so inject that actual mutation.
    let status = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(c.workspace.join("new-junction"))
        .arg(&external)
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::write(c.workspace.join("cleanup-release"), "go").unwrap();
    let result = task.await.unwrap().unwrap();
    assert_eq!(result["success"], true, "{result}");
    assert!(
        result["cleanup_incomplete"].as_u64().unwrap() > 0,
        "Skipped unsafe entries must be observable"
    );
    assert!(
        !has_appcontainer_entry(&c.workspace.join("new-file")),
        "A junction must not suppress cleanup of a safe sibling"
    );
    assert_eq!(
        std::fs::read_to_string(c.workspace.join("new-file")).unwrap(),
        "preserve this file"
    );
    assert_eq!(
        std::fs::read_to_string(external.join("secret")).unwrap(),
        "private"
    );
    assert_eq!(acl_entries(&external), external_acl);
    std::fs::remove_dir(c.workspace.join("new-junction")).unwrap();
}

#[tokio::test]
async fn windows_refuses_reparse_points_and_hard_links_before_granting() {
    let (temp, c, args) = setup();
    let outside = temp.path().join("outside");
    std::fs::write(&outside, "private").unwrap();
    std::fs::hard_link(&outside, c.workspace.join("link")).unwrap();
    assert!(
        tools::call(&c, &c.workspace, "workspace_run", &args)
            .await
            .is_err()
    );
    std::fs::remove_file(c.workspace.join("link")).unwrap();
    std::os::windows::fs::symlink_file(&outside, c.workspace.join("link"))
        .expect("Native Windows denial test requires symlink creation");
    assert!(
        tools::call(&c, &c.workspace, "workspace_run", &args)
            .await
            .is_err()
    );
    std::fs::remove_file(c.workspace.join("link")).unwrap();
    let external = temp.path().join("external-directory");
    std::fs::create_dir(&external).unwrap();
    std::fs::write(external.join("secret"), "private").unwrap();
    let junction = c.workspace.join("junction");
    let status = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&junction)
        .arg(&external)
        .status()
        .unwrap();
    assert!(status.success());
    assert!(
        tools::call(&c, &c.workspace, "workspace_run", &args)
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(external.join("secret")).unwrap(),
        "private"
    );
    std::fs::remove_dir(junction).unwrap();
}

#[tokio::test]
async fn windows_cancellation_and_timeout_end_descendants() {
    for cancel in [true, false] {
        let (_temp, mut c, args) = setup();
        if !cancel {
            c.limits.command_seconds = 1;
        }
        std::fs::write(
            c.workspace.join("command-probe.json"),
            "{\"operation\":\"wait\"}",
        )
        .unwrap();
        let work = c.workspace.clone();
        let mut task =
            tokio::spawn(
                async move { tools::call(&c, &c.workspace, "workspace_run", &args).await },
            );
        let started = work.join("descendant-started");
        tokio::select! {
            _ = wait_file(&started) => (),
            result = &mut task => panic!("Command ended before starting its descendant: {result:?}"),
        }
        if cancel {
            task.abort();
            let _ = task.await;
        } else {
            assert!(task.await.unwrap().is_err());
        }
        std::fs::write(work.join("release"), "go").unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !work.join("escaped").exists(),
            "Descendant survived command end"
        );
    }
}

#[tokio::test]
async fn windows_output_is_bounded_and_system_shell_runs_confined() {
    let (_temp, mut c, args) = setup();
    c.limits.tool_output_bytes = 4096;
    std::fs::write(
        c.workspace.join("command-probe.json"),
        "{\"operation\":\"output\"}",
    )
    .unwrap();
    let result = tools::call(&c, &c.workspace, "workspace_run", &args)
        .await
        .unwrap();
    assert_eq!(result["success"], true, "{result}");
    assert_eq!(result["truncated"], true);
    assert!(result["stdout"].as_str().unwrap().len() <= 4096);
    assert!(result["stderr"].as_str().unwrap().len() <= 4096);
    let result = tools::call(
        &c,
        &c.workspace,
        "workspace_run",
        &json!({"command":"cmd.exe","args":["/d","/c","echo native-shell"]}),
    )
    .await
    .unwrap();
    assert_eq!(result["success"], true, "{result}");
    assert!(
        result["stdout"].as_str().unwrap().contains("native-shell"),
        "{result}"
    );
}

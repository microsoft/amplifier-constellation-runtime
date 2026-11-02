//! A separate, explicitly authorized Sign in with ChatGPT registration.
//! Implements the public OSS flow, not Codex or ChatGPT private endpoints.
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const ISSUER: &str = "https://auth.openai.com";
const TOKEN: &str = "https://auth.openai.com/api/accounts/oauth/token";
const RESOURCE: &str = "https://api.openai.com/v1";
const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";

#[derive(Clone, Serialize, Deserialize)]
struct Credentials {
    schema_version: u32,
    client_id: String,
    issuer: String,
    subject: String,
    email: Option<String>,
    ext_agent_host_id: String,
    access_token: String,
    refresh_token: Option<String>,
    id_token: String,
    scopes: Vec<String>,
    expires_at: i64,
}
#[derive(Deserialize)]
struct Claims {
    sub: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    nonce: Option<String>,
}
fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()?)
}
fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
fn secret_random() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
pub fn path(home: &Path) -> PathBuf {
    home.join("accounts/chatgpt/credentials.json")
}
fn safe_auth_url(url: &str) -> Result<reqwest::Url> {
    let u = reqwest::Url::parse(url)?;
    ensure!(
        u.scheme() == "https"
            && u.host_str() == Some("auth.openai.com")
            && u.port().is_none()
            && u.username().is_empty()
            && u.password().is_none(),
        "OpenAI discovery returned an unexpected authentication origin"
    );
    Ok(u)
}
async fn response_json(response: reqwest::Response) -> Result<Value> {
    use futures_util::StreamExt;
    ensure!(
        response.status().is_success(),
        "ChatGPT authorization request failed (HTTP {}); reconnect this installation's account",
        response.status().as_u16()
    );
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("ChatGPT authorization response interrupted")?;
        ensure!(
            bytes.len() + chunk.len() <= 256 * 1024,
            "Authorization response too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).context("Invalid authorization response")
}
async fn discovery(client: &reqwest::Client) -> Result<Value> {
    response_json(
        client
            .get(format!("{ISSUER}/.well-known/openid-configuration"))
            .send()
            .await?,
    )
    .await
}
async fn verify(
    client: &reqwest::Client,
    token: &str,
    client_id: &str,
    nonce: Option<&str>,
) -> Result<Claims> {
    let metadata = discovery(client).await?;
    let jwks = safe_auth_url(
        metadata["jwks_uri"]
            .as_str()
            .context("OpenAI did not publish signing keys")?,
    )?;
    let keys: jsonwebtoken::jwk::JwkSet =
        serde_json::from_value(response_json(client.get(jwks).send().await?).await?)?;
    verify_with_keys(token, client_id, nonce, &keys)
}
fn verify_with_keys(
    token: &str,
    client_id: &str,
    nonce: Option<&str>,
    keys: &jsonwebtoken::jwk::JwkSet,
) -> Result<Claims> {
    let header = jsonwebtoken::decode_header(token).context("Invalid ID token")?;
    ensure!(
        header.alg == Algorithm::RS256,
        "Unsupported ID-token signing algorithm"
    );
    let jwk = keys
        .find(
            header
                .kid
                .as_deref()
                .context("ID token omitted its signing key")?,
        )
        .context("ID-token signing key is not published")?;
    let mut policy = Validation::new(Algorithm::RS256);
    policy.set_issuer(&[ISSUER]);
    policy.set_audience(&[client_id]);
    policy.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    let claims = jsonwebtoken::decode::<Claims>(token, &DecodingKey::from_jwk(jwk)?, &policy)
        .context("ChatGPT ID token could not be verified")?
        .claims;
    ensure!(!claims.sub.is_empty(), "ID token has no account identity");
    ensure!(
        nonce.is_none_or(|n| claims.nonce.as_deref() == Some(n)),
        "Sign-in nonce did not match"
    );
    Ok(claims)
}
fn lock(file: &Path) -> Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(file.with_extension("lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock)
        .context("This ChatGPT account is being changed in another process; try again")?;
    Ok(lock)
}
fn save(path: &Path, value: &impl Serialize) -> Result<()> {
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    crate::config::private_write(&temp, &serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(&temp, path)?;
    Ok(())
}
fn load(path: &Path) -> Result<Credentials> {
    let m = std::fs::metadata(path).context("Continue with ChatGPT for this installation first")?;
    ensure!(m.len() <= 64 * 1024, "Credential file too large");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            m.permissions().mode() & 0o077 == 0,
            "ChatGPT credentials must be readable only by their owner"
        );
    }
    let c: Credentials = serde_json::from_slice(&std::fs::read(path)?)
        .context("Not an Agent Runtime ChatGPT credential record")?;
    ensure!(
        c.schema_version == 1
            && c.issuer == ISSUER
            && c.client_id != "dynamic_agent_client"
            && !c.client_id.is_empty(),
        "Invalid ChatGPT account registration"
    );
    Ok(c)
}
fn sharing(c: &Credentials) -> bool {
    c.scopes.iter().any(|s| s == "chatgpt.tokens.use.direct")
}
fn public(c: &Credentials) -> Value {
    json!({"connected":true,"client_id":c.client_id,"subject":c.subject,"email":c.email,"sharing":sharing(c),"expires_at":c.expires_at,"usage_url":"https://chatgpt.com/settings/usage","credential_values_included":false})
}
fn apply_tokens(c: &mut Credentials, value: &Value) -> Result<()> {
    ensure!(
        value["token_type"]
            .as_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("bearer")),
        "Expected a bearer credential"
    );
    let access = value["access_token"]
        .as_str()
        .context("No access credential was returned")?;
    ensure!(
        !access.is_empty() && access.len() <= 32 * 1024 && !access.chars().any(char::is_control),
        "Invalid access credential"
    );
    c.access_token = access.to_owned();
    let expires = value["expires_in"]
        .as_i64()
        .context("No credential expiry was returned")?;
    ensure!(
        (1..=31 * 86400).contains(&expires),
        "Invalid credential expiry"
    );
    c.expires_at = now() + expires;
    if let Some(s) = value["refresh_token"].as_str() {
        c.refresh_token = Some(s.into());
    }
    if let Some(s) = value["id_token"].as_str() {
        c.id_token = s.into();
    }
    if let Some(s) = value["scope"].as_str() {
        c.scopes = s.split_whitespace().map(str::to_owned).collect();
    }
    Ok(())
}

pub fn status(home: &Path) -> Result<Value> {
    let file = path(home);
    if !file.exists() {
        return Ok(json!({"connected":false,"sharing":false}));
    }
    Ok(public(&load(&file)?))
}

pub async fn access_token(file: &Path) -> Result<String> {
    // Healthy tokens can be shared by simultaneous conversations. Serialize
    // refreshes and reload after locking so only one process rotates a token.
    let cached = load(file)?;
    if sharing(&cached) && cached.expires_at > now() + 120 {
        return Ok(cached.access_token);
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let _lock = loop {
        match lock(file) {
            Ok(lock) => break lock,
            Err(error) if tokio::time::Instant::now() >= deadline => return Err(error),
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    };
    let mut c = load(file)?;
    ensure!(
        sharing(&c),
        "This account has not authorized ChatGPT plan use; reconnect and grant that permission"
    );
    if c.expires_at <= now() + 120 {
        let refresh = c
            .refresh_token
            .as_deref()
            .context("ChatGPT sign-in expired; reconnect this installation")?;
        let client = client()?;
        let value = response_json(
            client
                .post(TOKEN)
                .form(&[
                    ("grant_type", "refresh_token"),
                    ("client_id", c.client_id.as_str()),
                    ("refresh_token", refresh),
                    ("resource", RESOURCE),
                ])
                .send()
                .await?,
        )
        .await?;
        if let Some(token) = value["id_token"].as_str() {
            let claims = verify(&client, token, &c.client_id, None).await?;
            ensure!(
                claims.sub == c.subject,
                "Renewed credentials belong to a different account"
            );
        }
        apply_tokens(&mut c, &value)?;
        save(file, &c)?;
        ensure!(
            sharing(&c),
            "ChatGPT plan permission was withdrawn; inference stopped"
        );
    }
    Ok(c.access_token)
}

fn callback_values(
    target: &str,
    expected_state: &str,
    registered: Option<&str>,
) -> Result<(String, String)> {
    let url = reqwest::Url::parse(&format!("http://127.0.0.1{target}"))?;
    ensure!(url.path() == "/auth/callback", "Unexpected callback path");
    let mut p = HashMap::new();
    for (k, v) in url.query_pairs() {
        ensure!(
            p.insert(k.into_owned(), v.into_owned()).is_none(),
            "Duplicate callback parameter"
        );
    }
    ensure!(
        p.get("state").is_some_and(|s| s == expected_state),
        "Sign-in state did not match"
    );
    ensure!(
        !p.contains_key("error"),
        "ChatGPT sign-in was declined or cancelled"
    );
    let client_id = p
        .get("client_id")
        .map(String::as_str)
        .or(registered)
        .context("Registration did not return a client ID")?;
    ensure!(
        !client_id.is_empty()
            && client_id != "dynamic_agent_client"
            && registered.is_none_or(|s| s == client_id),
        "Unexpected client registration"
    );
    Ok((
        p.get("code")
            .filter(|s| !s.is_empty())
            .context("No authorization code was returned")?
            .clone(),
        client_id.to_owned(),
    ))
}

pub async fn sign_in(home: &Path, port: u16, no_browser: bool) -> Result<Value> {
    let file = path(home);
    let folder = file.parent().unwrap();
    crate::config::private_dir(folder)?;
    let _lock = lock(&file)?;
    let host_path = folder.join("host.json");
    let host = if host_path.exists() {
        serde_json::from_slice::<Value>(&std::fs::read(&host_path)?)?["ext_agent_host_id"]
            .as_str()
            .context("Invalid host identity")?
            .to_owned()
    } else {
        let id = format!("urn:uuid:{}", uuid::Uuid::new_v4());
        save(&host_path, &json!({"ext_agent_host_id":id}))?;
        id
    };
    let registration_path = folder.join("registration.json");
    let registration = if registration_path.exists() {
        serde_json::from_slice::<Value>(&std::fs::read(&registration_path)?)?
    } else {
        json!({})
    };
    let registered = registration["client_id"].as_str();
    let registered_subject = registration["subject"].as_str();
    let previous = if file.exists() {
        Some(load(&file)?)
    } else {
        None
    };
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .context("Could not open the local sign-in callback; choose another --port")?;
    let callback = format!(
        "http://127.0.0.1:{}/auth/callback",
        listener.local_addr()?.port()
    );
    let state = secret_random();
    let nonce = secret_random();
    let verifier = secret_random();
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut url = reqwest::Url::parse(&format!("{ISSUER}/api/accounts/authorize"))?;
    url.query_pairs_mut().extend_pairs([
        ("client_id", registered.unwrap_or("dynamic_agent_client")),
        ("ext_agent_host_id", &host),
        ("response_type", "code"),
        ("redirect_uri", &callback),
        ("scope", SCOPES),
        ("resource", RESOURCE),
        ("state", &state),
        ("nonce", &nonce),
        ("code_challenge_method", "S256"),
        ("code_challenge", &challenge),
    ]);
    if registered.is_none() {
        url.query_pairs_mut()
            .append_pair("agent_name_hint", "Agent Runtime");
    }
    // This link contains only ephemeral PKCE/state material, not saved tokens.
    println!(
        "{}",
        json!({"action":"Continue with ChatGPT","authorization_url":url.as_str(),"callback":callback,"expires_in_seconds":300,"remote_host_note":"For a remote machine use an SSH local port forward, or sign in locally and use chatgpt-import over SSH."})
    );
    if !no_browser {
        let mut command = if cfg!(target_os = "macos") {
            std::process::Command::new("open")
        } else if cfg!(target_os = "windows") {
            let mut c = std::process::Command::new("rundll32.exe");
            c.arg("url.dll,FileProtocolHandler");
            c
        } else {
            std::process::Command::new("xdg-open")
        };
        let _ = command
            .arg(url.as_str())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    let received = tokio::time::timeout(Duration::from_secs(300), async {
        loop {
            let (mut stream, _) = listener.accept().await?;
            let mut bytes = Vec::new(); let mut block = [0;1024];
            while !bytes.windows(4).any(|v|v==b"\r\n\r\n") {
                let n = tokio::time::timeout(Duration::from_secs(5),stream.read(&mut block)).await??;
                ensure!(n > 0 && bytes.len()+n <= 8192, "Invalid callback request"); bytes.extend_from_slice(&block[..n]);
            }
            let request = String::from_utf8(bytes)?;
            let mut parts = request.lines().next().unwrap_or("").split_whitespace();
            let method = parts.next(); let target = parts.next().unwrap_or("");
            if method != Some("GET") || !target.starts_with("/auth/callback?") { stream.write_all(b"HTTP/1.1 404 Not Found\r\nConnection: close\r\nContent-Length: 0\r\n\r\n").await?; continue; }
            let result = callback_values(target, &state, registered);
            let message = if result.is_ok() { "Return to Agent Runtime to finish connecting." } else { "The sign-in callback was rejected. Return to Agent Runtime." };
            stream.write_all(format!("HTTP/1.1 {}\r\nContent-Type: text/plain\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}", if result.is_ok(){"200 OK"}else{"400 Bad Request"},message.len(),message).as_bytes()).await?;
            return result;
        }
    }).await.context("ChatGPT sign-in expired; start again")??;
    let (code, client_id) = received;
    // Preserve an established account binding through unsuccessful sign-ins
    // and sign-out; history must never move to another account implicitly.
    save(
        &registration_path,
        &json!({"client_id":client_id,"subject":registered_subject}),
    )?;
    let client = client()?;
    let value = response_json(
        client
            .post(TOKEN)
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", client_id.as_str()),
                ("code", code.as_str()),
                ("code_verifier", verifier.as_str()),
                ("redirect_uri", callback.as_str()),
                ("resource", RESOURCE),
            ])
            .send()
            .await?,
    )
    .await?;
    let token = value["id_token"]
        .as_str()
        .context("OpenAI did not return an identity token")?;
    let claims = verify(&client, token, &client_id, Some(&nonce)).await?;
    ensure!(
        previous
            .as_ref()
            .is_none_or(|c| c.subject == claims.sub && c.client_id == client_id)
            && registered_subject.is_none_or(|s| s == claims.sub),
        "Sign-in identity differs from this installation's account; use a separate runtime home"
    );
    let mut credentials = Credentials {
        schema_version: 1,
        client_id,
        issuer: ISSUER.into(),
        subject: claims.sub,
        email: claims.email,
        ext_agent_host_id: host,
        access_token: String::new(),
        refresh_token: None,
        id_token: String::new(),
        scopes: vec![],
        expires_at: 0,
    };
    apply_tokens(&mut credentials, &value)?;
    save(
        &registration_path,
        &json!({"client_id":credentials.client_id,"subject":credentials.subject}),
    )?;
    save(&file, &credentials)?;
    let mut result = public(&credentials);
    result["credentials_file"] = json!(file);
    Ok(result)
}

pub async fn models(home: &Path) -> Result<Value> {
    let token = access_token(&path(home)).await?;
    let value = response_json(
        client()?
            .get(format!("{RESOURCE}/models"))
            .bearer_auth(token)
            .send()
            .await?,
    )
    .await?;
    let list = value["models"]
        .as_array()
        .context("Account model catalog did not return models")?;
    Ok(
        json!({"models":list.iter().filter(|m| m["visibility"] == "list").map(|m|json!({"slug":m["slug"],"display_name":m["display_name"]})).collect::<Vec<_>>()}),
    )
}

pub async fn import(home: &Path, source: &Path) -> Result<Value> {
    let file = path(home);
    let folder = file.parent().unwrap();
    crate::config::private_dir(folder)?;
    let _lock = lock(&file)?;
    ensure!(
        !file.exists(),
        "This installation already has a ChatGPT account; it was preserved"
    );
    let mut c = load(source)?;
    let registration = folder.join("registration.json");
    if registration.exists() {
        let previous: Value = serde_json::from_slice(&std::fs::read(&registration)?)?;
        ensure!(
            previous["subject"].as_str().is_none_or(|s| s == c.subject)
                && previous["client_id"]
                    .as_str()
                    .is_none_or(|s| s == c.client_id),
            "This installation is bound to a different ChatGPT account; use a separate home"
        );
    }
    let claims = verify(&client()?, &c.id_token, &c.client_id, None).await?;
    ensure!(
        claims.sub == c.subject,
        "Imported registration does not match its verified account"
    );
    let host_path = folder.join("host.json");
    let host = if host_path.exists() {
        serde_json::from_slice::<Value>(&std::fs::read(&host_path)?)?["ext_agent_host_id"]
            .as_str()
            .context("Invalid host identity")?
            .to_owned()
    } else {
        format!("urn:uuid:{}", uuid::Uuid::new_v4())
    };
    c.ext_agent_host_id = host.clone();
    save(&host_path, &json!({"ext_agent_host_id":host}))?;
    save(
        &folder.join("registration.json"),
        &json!({"client_id":c.client_id,"subject":c.subject}),
    )?;
    save(&file, &c)?;
    Ok(public(&c))
}

pub async fn sign_out(home: &Path) -> Result<Value> {
    let file = path(home);
    let _lock = lock(&file)?;
    let c = load(&file)?;
    let client = client()?;
    let metadata = discovery(&client).await?;
    let url = safe_auth_url(
        metadata["revocation_endpoint"]
            .as_str()
            .context("OpenAI did not publish a revocation endpoint")?,
    )?;
    let response = client
        .post(url)
        .form(&[
            ("client_id", c.client_id.as_str()),
            (
                "token",
                c.refresh_token.as_deref().unwrap_or(&c.access_token),
            ),
            (
                "token_type_hint",
                if c.refresh_token.is_some() {
                    "refresh_token"
                } else {
                    "access_token"
                },
            ),
        ])
        .send()
        .await?;
    ensure!(
        response.status().is_success(),
        "ChatGPT sign-out could not revoke the account session (HTTP {}); credentials were preserved for retry",
        response.status().as_u16()
    );
    std::fs::remove_file(file)?;
    Ok(json!({"connected":false,"revoked":true,"registration_preserved":true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_binds_state_registration_and_rejects_duplicates() {
        assert_eq!(
            callback_values(
                "/auth/callback?code=x&state=s&client_id=oaiapp_test",
                "s",
                None
            )
            .unwrap(),
            ("x".into(), "oaiapp_test".into())
        );
        for target in [
            "/auth/callback?code=x&state=wrong&client_id=one",
            "/auth/callback?code=x&state=s&state=s&client_id=one",
            "/auth/callback?code=x&state=s&client_id=other",
            "/auth/callback?error=access_denied&state=s",
            "/auth/callback?code=x&state=s&client_id=dynamic_agent_client",
        ] {
            assert!(callback_values(target, "s", Some("original")).is_err());
        }
        assert_eq!(
            callback_values("/auth/callback?code=x&state=s", "s", Some("original"))
                .unwrap()
                .1,
            "original"
        );
    }
    #[test]
    fn metadata_cannot_redirect_credentials_elsewhere() {
        for value in [
            "http://auth.openai.com/token",
            "https://evil.example/token",
            "https://auth.openai.com.evil.example/token",
            "https://auth.openai.com:8443/token",
            "https://user@auth.openai.com/token",
        ] {
            assert!(safe_auth_url(value).is_err());
        }
        assert!(safe_auth_url("https://auth.openai.com/jwks").is_ok());
    }
    #[test]
    fn identity_verification_requires_signature_expiry_audience_issuer_and_nonce() {
        // This generated key is a public test fixture, never an account key.
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(include_bytes!(
            "../tests/fixtures/oidc-test-private.pem"
        ))
        .unwrap();
        let keys =
            serde_json::from_str(include_str!("../tests/fixtures/oidc-test-jwks.json")).unwrap();
        let mut header = jsonwebtoken::Header::new(Algorithm::RS256);
        header.kid = Some("test-only".into());
        let valid = json!({"sub":"test-user","aud":"oaiapp_test","iss":ISSUER,"exp":now()+300,"nonce":"test-nonce"});
        let token = jsonwebtoken::encode(&header, &valid, &key).unwrap();
        assert_eq!(
            verify_with_keys(&token, "oaiapp_test", Some("test-nonce"), &keys)
                .unwrap()
                .sub,
            "test-user"
        );
        assert!(verify_with_keys(&token, "other-client", Some("test-nonce"), &keys).is_err());
        assert!(verify_with_keys(&token, "oaiapp_test", Some("wrong"), &keys).is_err());
        for (field, value) in [
            ("exp", json!(now() - 300)),
            ("iss", json!("https://example.com")),
            ("sub", json!("")),
        ] {
            let mut bad = valid.clone();
            bad[field] = value;
            assert!(
                verify_with_keys(
                    &jsonwebtoken::encode(&header, &bad, &key).unwrap(),
                    "oaiapp_test",
                    Some("test-nonce"),
                    &keys
                )
                .is_err()
            );
        }
    }
}

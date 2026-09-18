//! Signed, platform-specific tray binary updater.

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ed25519_dalek::{Signature, VerifyingKey};
use reqwest::{Client, StatusCode, Url};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{fs, io::AsyncWriteExt};

const MANIFEST_LIMIT: u64 = 1024 * 1024;
const ARTIFACT_LIMIT: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UpdateConfig {
    pub manifest_url: String,
    pub public_key: String,
}

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub notes: String,
    artifact: Artifact,
}

#[derive(Clone, Debug, Deserialize)]
struct Envelope {
    payload: String,
    signature: String,
}

#[derive(Clone, Debug, Deserialize)]
struct Manifest {
    version: String,
    #[serde(default)]
    notes: String,
    artifacts: BTreeMap<String, Artifact>,
}

#[derive(Clone, Debug, Deserialize)]
struct Artifact {
    url: String,
    sha256: String,
    size: u64,
}

/// Check a signed manifest and return a release for this platform, if newer.
pub async fn check(config: &UpdateConfig, current_version: &str) -> Result<Option<Release>> {
    let current = Version::parse(current_version).context("invalid current version")?;
    let manifest_url = parse_initial_url(&config.manifest_url)?;
    let client = http_client(&manifest_url)?;
    let response = client
        .get(manifest_url)
        .send()
        .await
        .context("fetch manifest")?;
    if response.status() != StatusCode::OK {
        bail!("manifest request failed with HTTP {}", response.status());
    }
    let payload = read_limited(response, MANIFEST_LIMIT).await?;
    let envelope: Envelope =
        serde_json::from_slice(&payload).context("decode manifest envelope")?;
    let signed_payload = BASE64
        .decode(envelope.payload)
        .context("decode manifest payload")?;
    let signature = BASE64
        .decode(envelope.signature)
        .context("decode manifest signature")?;
    let key_bytes = hex::decode(&config.public_key).context("decode manifest public key")?;
    let key_bytes: [u8; 32] = key_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("public key must be 32 bytes"))?;
    let key = VerifyingKey::from_bytes(&key_bytes).context("invalid manifest public key")?;
    let signature = Signature::from_slice(&signature).context("invalid manifest signature")?;
    key.verify_strict(&signed_payload, &signature)
        .context("manifest signature verification failed")?;
    let manifest: Manifest =
        serde_json::from_slice(&signed_payload).context("decode signed manifest payload")?;
    let version = Version::parse(&manifest.version).context("invalid release version")?;
    if version <= current || !version.pre.is_empty() {
        return Ok(None);
    }
    let target = platform_target()?;
    let artifact = manifest
        .artifacts
        .get(target)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no artifact for platform {target}"))?;
    validate_artifact(&artifact)?;
    Ok(Some(Release {
        version: manifest.version,
        notes: manifest.notes,
        artifact,
    }))
}

/// Download and verify a previously validated release into `directory`.
pub async fn stage(_config: &UpdateConfig, release: &Release, directory: &Path) -> Result<PathBuf> {
    validate_artifact(&release.artifact)?;
    let artifact_url = parse_artifact_url(&release.artifact.url)?;
    let client = http_client(&artifact_url)?;
    fs::create_dir_all(directory)
        .await
        .context("create staging directory")?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let safe_version: String = release
        .version
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let final_path = directory.join(format!("peercarry-tray-{safe_version}-{nonce}"));
    let temp_path = directory.join(format!(".peercarry-tray-{safe_version}-{nonce}.part"));
    let result = download_artifact(&client, &release.artifact, &temp_path, &final_path).await;
    if result.is_err() {
        let _ = fs::remove_file(&temp_path).await;
    }
    result
}

async fn download_artifact(
    client: &Client,
    artifact: &Artifact,
    temp: &Path,
    final_path: &Path,
) -> Result<PathBuf> {
    let mut response = client
        .get(parse_artifact_url(&artifact.url)?)
        .send()
        .await
        .context("fetch artifact")?;
    if response.status() != StatusCode::OK {
        bail!("artifact request failed with HTTP {}", response.status());
    }
    if response
        .content_length()
        .is_some_and(|n| n > ARTIFACT_LIMIT || n != artifact.size)
    {
        bail!("artifact content length does not match declared size");
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp)
        .await
        .context("create temporary artifact")?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let outcome = async {
        while let Some(chunk) = response.chunk().await.context("read artifact")? {
            total = total
                .checked_add(chunk.len() as u64)
                .ok_or_else(|| anyhow::anyhow!("artifact size overflow"))?;
            if total > ARTIFACT_LIMIT || total > artifact.size {
                bail!("artifact exceeds declared size limit");
            }
            hasher.update(&chunk);
            file.write_all(&chunk).await.context("write artifact")?;
        }
        file.flush().await.context("flush artifact")?;
        if total != artifact.size {
            bail!("artifact is truncated or has unexpected size");
        }
        let actual = hex::encode(hasher.finalize());
        if !actual.eq_ignore_ascii_case(&artifact.sha256) {
            bail!("artifact sha256 mismatch");
        }
        drop(file);
        fs::rename(temp, final_path)
            .await
            .context("commit staged artifact")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(final_path, std::fs::Permissions::from_mode(0o755))
                .await
                .context("set executable mode")?;
        }
        Ok(final_path.to_path_buf())
    }
    .await;
    if outcome.is_err() {
        let _ = fs::remove_file(temp).await;
    }
    outcome
}

async fn read_limited(response: reqwest::Response, limit: u64) -> Result<Vec<u8>> {
    if response.content_length().is_some_and(|n| n > limit) {
        bail!("manifest exceeds size limit");
    }
    let mut out = Vec::new();
    let mut response = response;
    while let Some(chunk) = response.chunk().await.context("read manifest")? {
        if (out.len() as u64).saturating_add(chunk.len() as u64) > limit {
            bail!("manifest exceeds size limit");
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

fn http_client(initial: &Url) -> Result<Client> {
    let initial = initial.clone();
    Ok(Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(600))
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            let next = attempt.url();
            if attempt.previous().len() >= 3 {
                return attempt.error("too many redirects");
            }
            if redirect_allowed(&initial, next) {
                attempt.follow()
            } else {
                attempt.error("redirect target is not allowed")
            }
        }))
        .build()?)
}

fn parse_initial_url(value: &str) -> Result<Url> {
    let url = parse_url(value)?;
    if url.query().is_some() {
        bail!("manifest URL must not contain a query");
    }
    if url.scheme() != "https" && !is_loopback_http(&url) {
        bail!("update URLs must use HTTPS (or loopback HTTP)");
    }
    Ok(url)
}

fn parse_artifact_url(value: &str) -> Result<Url> {
    let url = parse_url(value)?;
    if url.scheme() != "https" && !is_loopback_http(&url) {
        bail!("update URLs must use HTTPS (or loopback HTTP)");
    }
    Ok(url)
}

fn parse_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).context("invalid update URL")?;
    if !url.username().is_empty() || url.password().is_some() {
        bail!("update URLs must not contain userinfo");
    }
    Ok(url)
}

fn is_loopback_http(url: &Url) -> bool {
    url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "::1"))
}

fn redirect_allowed(initial: &Url, next: &Url) -> bool {
    if next.username() != "" || next.password().is_some() {
        return false;
    }
    if initial.scheme() == "https" {
        if next.scheme() != "https" {
            return false;
        }
    } else if !(is_loopback_http(initial) && is_loopback_http(next)) {
        return false;
    }
    let Some(initial_host) = initial.host_str() else {
        return false;
    };
    let Some(next_host) = next.host_str() else {
        return false;
    };
    next_host.eq_ignore_ascii_case(initial_host)
        || (initial_host.eq_ignore_ascii_case("github.com")
            && matches!(
                next_host.to_ascii_lowercase().as_str(),
                "release-assets.githubusercontent.com" | "objects.githubusercontent.com"
            ))
}

fn validate_artifact(artifact: &Artifact) -> Result<()> {
    if artifact.size == 0 || artifact.size > ARTIFACT_LIMIT {
        bail!("artifact size is outside allowed limits");
    }
    if artifact.sha256.len() != 64 || !artifact.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid artifact sha256");
    }
    parse_artifact_url(&artifact.url)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::json;
    use std::sync::Arc;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[test]
    fn redirects_allow_github_release_asset_only() {
        let initial = Url::parse("https://github.com/acme/app/releases/download/v1/app").unwrap();
        assert!(redirect_allowed(
            &initial,
            &Url::parse("https://release-assets.githubusercontent.com/a?sig=x").unwrap()
        ));
        assert!(redirect_allowed(
            &initial,
            &Url::parse("https://objects.githubusercontent.com/a?sig=x").unwrap()
        ));
        assert!(!redirect_allowed(
            &initial,
            &Url::parse("https://evil.example/a").unwrap()
        ));
    }

    #[test]
    fn redirects_reject_downgrade_and_userinfo() {
        let initial = Url::parse("https://example.com/manifest").unwrap();
        assert!(!redirect_allowed(
            &initial,
            &Url::parse("http://example.com/a").unwrap()
        ));
        assert!(!redirect_allowed(
            &initial,
            &Url::parse("https://user:pass@example.com/a").unwrap()
        ));
    }

    #[test]
    fn initial_manifest_rejects_query_but_artifact_allows_it() {
        assert!(parse_initial_url("https://example.com/manifest?a=1").is_err());
        assert!(parse_artifact_url("https://example.com/a?sig=1").is_ok());
        assert!(parse_artifact_url("https://user@example.com/a").is_err());
    }

    fn signed_manifest(
        key: &SigningKey,
        version: &str,
        artifact_url: &str,
        sha256: &str,
        size: u64,
        target: Option<&str>,
    ) -> Vec<u8> {
        let mut artifacts = serde_json::Map::new();
        artifacts.insert(
            target.unwrap_or(platform_target().unwrap()).to_owned(),
            json!({
                "url": artifact_url, "sha256": sha256, "size": size
            }),
        );
        let payload = serde_json::to_vec(&json!({
            "version": version, "notes": "test", "artifacts": artifacts
        }))
        .unwrap();
        let signature = key.sign(&payload);
        serde_json::to_vec(&json!({
            "payload": BASE64.encode(payload), "signature": BASE64.encode(signature.to_bytes())
        }))
        .unwrap()
    }

    struct MockServer {
        task: tokio::task::JoinHandle<()>,
    }

    async fn test_fixture(
        version: &str,
        artifact: &[u8],
        requests: usize,
        content_length: bool,
    ) -> (UpdateConfig, MockServer) {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let sha = hex::encode(Sha256::digest(artifact));
        // The port is not known until the listener exists, so create a provisional server
        // first and sign with its actual artifact URL.
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let artifact_url = format!("http://127.0.0.1:{port}/artifact");
        let manifest = signed_manifest(
            &key,
            version,
            &artifact_url,
            &sha,
            artifact.len() as u64,
            None,
        );
        let manifest = Arc::new(manifest);
        let artifact = Arc::new(artifact.to_vec());
        let task = tokio::spawn(async move {
            for _ in 0..requests {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut request = vec![0u8; 4096];
                let n = stream.read(&mut request).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..n]);
                let body: &[u8] = if request.starts_with("GET /artifact") {
                    artifact.as_slice()
                } else {
                    manifest.as_slice()
                };
                let mut response = format!("HTTP/1.1 200 OK\r\nConnection: close\r\n");
                if content_length || request.starts_with("GET /manifest") {
                    response.push_str(&format!("Content-Length: {}\r\n", body.len()));
                }
                response.push_str("\r\n");
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.write_all(body).await;
            }
        });
        let config = UpdateConfig {
            manifest_url: format!("http://127.0.0.1:{port}/manifest"),
            public_key: hex::encode(key.verifying_key().to_bytes()),
        };
        (config, MockServer { task })
    }

    #[tokio::test]
    async fn check_and_stage_signed_artifact() {
        let data = b"tray-binary";
        let (config, server) = test_fixture("1.2.0", data, 2, true).await;
        let release = check(&config, "1.1.0").await.unwrap().unwrap();
        assert_eq!(release.version, "1.2.0");
        let dir =
            std::env::temp_dir().join(format!("peercarry-updater-test-{}", std::process::id()));
        let path = stage(&config, &release, &dir).await.unwrap();
        assert_eq!(fs::read(&path).await.unwrap(), data);
        let _ = fs::remove_file(path).await;
        let _ = fs::remove_dir(dir).await;
        server.task.await.unwrap();
    }

    #[tokio::test]
    async fn stage_rejects_hash_and_truncated_artifact() {
        let data = b"tray-binary";
        let (config, server) = test_fixture("1.2.0", data, 1, true).await;
        let release = check(&config, "1.1.0").await.unwrap().unwrap();
        let mut bad = release.clone();
        bad.artifact.sha256 = "00".repeat(32);
        assert!(stage(&config, &bad, &std::env::temp_dir()).await.is_err());
        server.task.await.unwrap();

        let (config, server) = test_fixture("1.2.0", data, 2, false).await;
        let release = check(&config, "1.1.0").await.unwrap().unwrap();
        let mut truncated = release.clone();
        truncated.artifact.size += 1;
        assert!(stage(&config, &truncated, &std::env::temp_dir())
            .await
            .is_err());
        server.task.await.unwrap();
    }

    #[tokio::test]
    async fn check_rejects_bad_signature_and_filters_versions_and_platform() {
        let data = b"tray-binary";
        let (config, server) = test_fixture("1.2.0", data, 1, true).await;
        let mut bad = config.clone();
        bad.public_key = hex::encode([8u8; 32]);
        assert!(check(&bad, "1.1.0").await.is_err());
        server.task.await.unwrap();

        let (config, server) = test_fixture("1.0.0", data, 1, true).await;
        assert!(check(&config, "1.1.0").await.unwrap().is_none());
        server.task.await.unwrap();
        let (config, server) = test_fixture("1.2.0-beta.1", data, 1, true).await;
        assert!(check(&config, "1.1.0").await.unwrap().is_none());
        server.task.await.unwrap();

        let key = SigningKey::from_bytes(&[7u8; 32]);
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let manifest = signed_manifest(
            &key,
            "1.2.0",
            &format!("http://127.0.0.1:{port}/artifact"),
            &hex::encode(Sha256::digest(data)),
            data.len() as u64,
            Some("unsupported-target"),
        );
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                manifest.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.write_all(&manifest).await.unwrap();
        });
        let config = UpdateConfig {
            manifest_url: format!("http://127.0.0.1:{port}/manifest"),
            public_key: hex::encode(key.verifying_key().to_bytes()),
        };
        assert!(check(&config, "1.1.0").await.is_err());
        task.await.unwrap();
    }
}

fn platform_target() -> Result<&'static str> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        return Ok("x86_64-pc-windows-msvc");
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        return Ok("aarch64-pc-windows-msvc");
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        return Ok("x86_64-unknown-linux-gnu");
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        return Ok("aarch64-unknown-linux-gnu");
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        return Ok("x86_64-apple-darwin");
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        return Ok("aarch64-apple-darwin");
    }
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
    )))]
    {
        bail!("unsupported updater platform")
    }
}

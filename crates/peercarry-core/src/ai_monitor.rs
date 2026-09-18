//! Bounded peer polling and explicitly enabled local AI alerts. No model calls.
use crate::{config::Config, paths, protocol};

pub async fn local_gate(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let local = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .is_some_and(|c| c.0.ip().is_loopback());
    let host = request
        .headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let authority = host.parse::<axum::http::uri::Authority>().ok();
    let allowed = authority.as_ref().is_some_and(|a| {
        a.host() == "localhost"
            || a.host()
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    let origin_ok = request
        .headers()
        .get("origin")
        .map(|v| v.to_str().ok() == Some(format!("http://{host}").as_str()))
        .unwrap_or(true);
    if !local || !allowed || !origin_ok {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
}
use axum::{
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use futures_util::{stream, StreamExt};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

#[derive(Default)]
struct Monitor {
    initialized: bool,
    dirty: bool,
    enabled: bool,
    seen: VecDeque<String>,
    devices: BTreeMap<String, Value>,
    alerts: VecDeque<(String, String)>,
    discovery_error: Option<String>,
}
static MONITOR: OnceLock<Mutex<Monitor>> = OnceLock::new();
fn monitor() -> &'static Mutex<Monitor> {
    MONITOR.get_or_init(|| Mutex::new(Monitor::default()))
}
fn initialize(m: &mut Monitor) {
    if m.initialized {
        return;
    }
    if let Ok(bytes) = std::fs::read(paths::data_dir().join("ai-monitor.json")) {
        if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
            m.enabled = v["enabled"].as_bool().unwrap_or(false);
            m.seen = v["seen"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .take(8192)
                .collect();
            m.alerts = serde_json::from_value(v["alerts"].clone()).unwrap_or_default();
            m.alerts.truncate(32);
        }
    }
    m.initialized = true;
}
fn save(m: &mut Monitor) -> std::io::Result<()> {
    if !m.dirty {
        return Ok(());
    }
    std::fs::create_dir_all(paths::data_dir())?;
    let temp = paths::data_dir().join("ai-monitor.json.tmp");
    std::fs::write(
        &temp,
        serde_json::to_vec(&json!({"enabled":m.enabled,"seen":m.seen,"alerts":m.alerts}))?,
    )?;
    std::fs::rename(temp, paths::data_dir().join("ai-monitor.json"))?;
    m.dirty = false;
    Ok(())
}
pub async fn overview() -> impl IntoResponse {
    let mut m = monitor().lock().unwrap();
    initialize(&mut m);
    Json(
        json!({"receiver_enabled":m.enabled,"devices":m.devices.values().collect::<Vec<_>>(),"discovery_error":m.discovery_error}),
    )
}
pub async fn receiver(headers: HeaderMap, Json(body): Json<Value>) -> impl IntoResponse {
    // JSON-only endpoint; cross-origin browser POSTs must not toggle alerts.
    if headers.contains_key("origin")
        && !headers
            .get("sec-fetch-site")
            .is_some_and(|v| v == "same-origin")
    {
        return (StatusCode::FORBIDDEN, "same-origin request required");
    }
    let Some(enabled) = body["enabled"].as_bool() else {
        return (StatusCode::BAD_REQUEST, "enabled must be boolean");
    };
    let mut m = monitor().lock().unwrap();
    initialize(&mut m);
    let old = (m.enabled, m.alerts.clone(), m.dirty);
    m.dirty |= m.enabled != enabled || (!enabled && !m.alerts.is_empty());
    m.enabled = enabled;
    if !enabled {
        m.alerts.clear();
    }
    if save(&mut m).is_err() {
        m.enabled = old.0;
        m.alerts = old.1;
        m.dirty = old.2;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "cannot save receiver setting",
        );
    }
    (StatusCode::NO_CONTENT, "")
}

fn cli_command() -> anyhow::Result<tokio::process::Command> {
    let executable = std::env::current_exe()?;
    let cli = executable
        .parent()
        .ok_or_else(|| anyhow::anyhow!("no executable directory"))?
        .join(if cfg!(windows) {
            "peercarry.exe"
        } else {
            "peercarry"
        });
    if !cli.is_file() {
        anyhow::bail!("Install peercarry alongside the tray before binding tools");
    }
    let mut command = tokio::process::Command::new(cli);
    command.kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    Ok(command)
}
pub async fn tools() -> axum::response::Response {
    let result = async {
        let output = tokio::time::timeout(
            Duration::from_secs(5),
            cli_command()?.args(["ai", "scan"]).output(),
        )
        .await??;
        anyhow::ensure!(output.status.success(), "tool scan failed");
        Ok::<Value, anyhow::Error>(serde_json::from_slice(&output.stdout)?)
    }
    .await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"message":error.to_string()})),
        )
            .into_response(),
    }
}
pub async fn bind_tool(headers: HeaderMap, Json(body): Json<Value>) -> axum::response::Response {
    if headers.contains_key("origin")
        && !headers
            .get("sec-fetch-site")
            .is_some_and(|v| v == "same-origin")
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let tool = body["tool"].as_str().unwrap_or("");
    if !matches!(tool, "codex" | "zcode") {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let remove = body["remove"].as_bool().unwrap_or(false);
    let result = async {
        let mut command = cli_command()?;
        command.args(["ai", "bind", tool]);
        if remove {
            command.arg("--remove");
        }
        let output = tokio::time::timeout(Duration::from_secs(10), command.output()).await??;
        anyhow::ensure!(
            output.status.success(),
            "Binding failed; check existing hook configuration and file permissions"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    match result {
        Ok(())=>Json(json!({"message":if remove {"已移除本工具的绑定 / Binding removed"}else{"配置已写入；请在工具中审核信任，并新建会话验证 / Configuration saved; review trust and verify in a new session"}})).into_response(),
        Err(error)=>(StatusCode::INTERNAL_SERVER_ERROR,Json(json!({"message":error.to_string()}))).into_response()
    }
}
pub fn take_alert() -> Option<String> {
    let mut m = monitor().lock().ok()?;
    if m.enabled {
        let alert = m.alerts.pop_front().map(|(_, text)| text);
        if alert.is_some() {
            m.dirty = true;
            let _ = save(&mut m);
        }
        alert
    } else {
        None
    }
}
fn alert_label(event: &str) -> Option<&'static str> {
    match event {
        "Stop" => Some("本轮结束信号 / Turn stopped"),
        "PermissionRequest" => Some("请求授权 / Approval requested"),
        _ => None,
    }
}
fn record(m: &mut Monitor, key: String, host: &str, events: Vec<Value>) {
    let previous_alerts = m.alerts.clone();
    let mut latest = BTreeMap::new();
    // Feed is ordered server-side, but compare sequence explicitly.
    for event in &events {
        let session = format!("{}:{}", event["tool"], event["session_id"]);
        let order = event["sequence"].as_u64().unwrap_or(0);
        let current: &mut (u64, &Value) = latest.entry(session).or_insert((0, event));
        if order >= current.0 {
            *current = (order, event);
        }
    }
    let mut seen: HashSet<_> = m.seen.iter().cloned().collect();
    for (_, event) in latest.values() {
        let session = format!("{}:{}:{}", key, event["tool"], event["session_id"]);
        if event["hook"].as_str().and_then(alert_label).is_none() {
            m.alerts.retain(|(s, _)| s != &session);
        }
        let id = format!("{}:{}", key, event["id"].as_str().unwrap_or(""));
        if !seen.contains(&id) && m.enabled {
            if let Some(label) = event["hook"].as_str().and_then(alert_label) {
                let text = format!(
                    "{} · {} · {}\n{}",
                    host,
                    event["tool"].as_str().unwrap_or("AI"),
                    event["project"].as_str().unwrap_or(""),
                    label
                );
                m.alerts.retain(|(s, _)| s != &session);
                if m.alerts.len() < 32 {
                    m.alerts.push_back((session, text));
                }
            }
        }
    }
    for event in &events {
        let id = format!("{}:{}", key, event["id"].as_str().unwrap_or(""));
        if seen.insert(id.clone()) {
            m.dirty = true;
            m.seen.push_back(id);
        }
    }
    while m.seen.len() > 8192 {
        m.seen.pop_front();
    }
    m.dirty |= m.alerts != previous_alerts;
    m.devices
        .insert(key, json!({"host":host,"online":true,"events":events}));
}
fn record_failure(m: &mut Monitor, key: String, host: &str) {
    let device = m
        .devices
        .entry(key)
        .or_insert_with(|| json!({"host":host,"events":[]}));
    device["online"] = json!(false);
    device["error"] = json!("????????????????????? / Check version, network and shared auth token");
}
async fn fetch(
    client: &reqwest::Client,
    config: &Config,
    address: &str,
) -> anyhow::Result<Vec<Value>> {
    let mut request = client.get(format!("{address}/v1/ai/events"));
    if let Some(token) = &config.network.auth_token {
        request = request.header(protocol::TOKEN_HEADER, token);
    }
    if let Some(token) = &config.ai_token {
        request = request.header("x-syncclip-ai-token", token);
    }
    let mut response = request.send().await?.error_for_status()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > 2 * 1024 * 1024 {
            anyhow::bail!("event feed too large");
        }
        bytes.extend_from_slice(&chunk);
    }
    let events: Vec<Value> = serde_json::from_slice(&bytes)?;
    if events.len() > 1000 {
        anyhow::bail!("too many events");
    }
    Ok(events)
}
pub async fn run(config: Arc<Config>) {
    let Ok(client) = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(3))
        .build()
    else {
        return;
    };
    loop {
        let node = config.display_name();
        let _ = tokio::task::spawn_blocking(move || {
            crate::ai_tasks::drain_outbox(&paths::data_dir(), node)
        })
        .await;
        let local = fetch(
            &client,
            &config,
            &format!("http://127.0.0.1:{}", config.network.port),
        )
        .await;
        {
            let mut m = monitor().lock().unwrap();
            initialize(&mut m);
            match local {
                Ok(events) => record(&mut m, "local".into(), &config.display_name(), events),
                Err(_) => record_failure(&mut m, "local".into(), &config.display_name()),
            }
        }
        let peers = tokio::time::timeout(
            Duration::from_secs(4),
            crate::tailscale::peers(config.network.port),
        )
        .await;
        let peers = match peers {
            Ok(Ok(p)) => {
                monitor().lock().unwrap().discovery_error = None;
                p
            }
            _ => {
                let mut m = monitor().lock().unwrap();
                m.discovery_error =
                    Some("设备发现失败；远端状态未知 / Peer discovery unavailable".into());
                for (key, device) in &mut m.devices {
                    if key != "local" {
                        device["online"] = json!(false);
                    }
                }
                Vec::new()
            }
        };
        let results = stream::iter(peers.into_iter().take(64).map(|peer| {
            let client = &client;
            let config = &config;
            async move {
                let address = peer.addr_with_port(config.network.port);
                let result = if !peer.online {
                    Err(anyhow::anyhow!("设备离线 / Device offline"))
                } else if let Some(address) = address {
                    fetch(client, config, &address).await
                } else {
                    Err(anyhow::anyhow!("没有设备地址 / Missing address"))
                };
                (peer.host, result)
            }
        }))
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
        {
            let mut m = monitor().lock().unwrap();
            for (key, device) in &mut m.devices {
                if key != "local" {
                    device["online"] = json!(false);
                }
            }
            for (host, result) in results {
                match result {
                    Ok(events) => record(&mut m, host.clone(), &host, events),
                    Err(_) => record_failure(&mut m, host.clone(), &host),
                }
            }
            if let Err(error) = save(&mut m) {
                tracing::warn!("AI notification state save failed: {error}");
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unchanged_feed_does_not_need_persistence() {
        let mut m = Monitor::default();
        let event = json!({"id":"a","tool":"codex","session_id":"s","hook":"Stop","sequence":1});
        record(
            &mut m,
            "local".into(),
            "pc",
            vec![event.clone(), event.clone()],
        );
        assert!(m.dirty);
        assert_eq!(m.seen.len(), 1);
        m.dirty = false;
        record(&mut m, "local".into(), "pc", vec![event]);
        assert!(!m.dirty);
        assert_eq!(m.seen.len(), 1);
    }

    #[test]
    fn fetch_failure_marks_local_offline_and_recovery_clears_error() {
        let mut m = Monitor::default();
        record(&mut m, "local".into(), "pc", vec![]);
        assert_eq!(m.devices["local"]["online"], true);
        record_failure(&mut m, "local".into(), "pc");
        assert_eq!(m.devices["local"]["online"], false);
        assert!(m.devices["local"]["error"].is_string());
        assert!(!m.dirty);
        record(&mut m, "local".into(), "pc", vec![]);
        assert_eq!(m.devices["local"]["online"], true);
        assert!(m.devices["local"].get("error").is_none());
    }

    #[test]
    fn dedup_and_resumed_session_do_not_alert() {
        let mut m = Monitor {
            enabled: true,
            ..Default::default()
        };
        let e = |id, event, seq| json!({"id":id,"tool":"codex","session_id":"s","hook":event,"sequence":seq});
        record(
            &mut m,
            "pc".into(),
            "pc",
            vec![e("a", "PermissionRequest", 1), e("b", "PostToolUse", 2)],
        );
        assert!(m.alerts.is_empty());
        record(&mut m, "pc".into(), "pc", vec![e("c", "Stop", 3)]);
        assert_eq!(m.alerts.len(), 1);
        record(&mut m, "pc".into(), "pc", vec![e("c", "Stop", 3)]);
        assert_eq!(m.alerts.len(), 1);
    }
}

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
use crate::server::AppState;
use axum::extract::State;
use axum::{
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use futures_util::{stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ReceiverSelection {
    pub device_id: String,
    pub display_name: String,
    pub counter: u64,
    pub operation_id: String,
    pub enabled: bool,
    pub group: String,
}
fn selection_order(a: &ReceiverSelection, b: &ReceiverSelection) -> std::cmp::Ordering {
    (a.counter, &a.operation_id).cmp(&(b.counter, &b.operation_id))
}
fn group_id(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

#[derive(Default)]
struct Monitor {
    initialized: bool,
    dirty: bool,
    enabled: bool,
    seen: VecDeque<String>,
    devices: BTreeMap<String, Value>,
    alerts: VecDeque<(String, String)>,
    discovery_error: Option<String>,
    device_id: String,
    selection: Option<ReceiverSelection>,
    sync_devices: BTreeMap<String, String>,
    sync_configured: bool,
    sync_group: Option<String>,
    baseline_sources: HashSet<String>,
    baseline_pending: HashSet<String>,
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
            m.device_id = v["device_id"]
                .as_str()
                .filter(|s| uuid::Uuid::parse_str(s).is_ok())
                .unwrap_or("")
                .to_owned();
            m.selection = serde_json::from_value(v["selection"].clone()).ok();
            m.sync_configured = v["sync_configured"].as_bool().unwrap_or(false);
            m.sync_group = v["sync_group"].as_str().map(str::to_owned);
            m.baseline_sources =
                serde_json::from_value(v["baseline_sources"].clone()).unwrap_or_default();
            m.baseline_pending =
                serde_json::from_value(v["baseline_pending"].clone()).unwrap_or_default();
        }
    }
    if m.device_id.is_empty() {
        m.device_id = uuid::Uuid::new_v4().to_string();
        m.dirty = true;
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
        serde_json::to_vec(
            &json!({"enabled":m.enabled,"seen":m.seen,"alerts":m.alerts,"device_id":m.device_id,"selection":m.selection,"sync_configured":m.sync_configured,"sync_group":m.sync_group,"baseline_sources":m.baseline_sources,"baseline_pending":m.baseline_pending}),
        )?,
    )?;
    std::fs::rename(temp, paths::data_dir().join("ai-monitor.json"))?;
    m.dirty = false;
    Ok(())
}
pub async fn overview(State(state): State<AppState>) -> impl IntoResponse {
    let mut m = monitor().lock().unwrap();
    initialize(&mut m);
    let old = (
        m.enabled,
        m.alerts.clone(),
        m.selection.clone(),
        m.sync_devices.clone(),
        m.sync_configured,
        m.dirty,
        m.baseline_pending.clone(),
        m.sync_group.clone(),
    );
    let group = state
        .config
        .ai_token
        .as_deref()
        .filter(|t| t.len() >= 32)
        .map(group_id);
    let group_changed = m.sync_group != group;
    let stale_selection = m
        .selection
        .as_ref()
        .is_some_and(|s| group.as_ref().is_none_or(|g| &s.group != g));
    if stale_selection || group_changed {
        if stale_selection || group.is_none() {
            m.selection = None;
            m.enabled = false;
            m.alerts.clear();
        }
        if group_changed && m.selection.is_none() && m.enabled {
            m.enabled = false;
            m.alerts.clear();
        }
        m.sync_group = group.clone();
        m.baseline_pending.clear();
        m.sync_devices.clear();
        m.sync_configured = group.is_some();
        m.dirty = true;
    } else {
        m.sync_configured = group.is_some();
        if group.is_some() && !old.4 && m.selection.is_none() && m.enabled {
            m.enabled = false;
            m.alerts.clear();
            m.dirty = true;
        }
    }
    m.dirty |= m.sync_configured != old.4;
    if save(&mut m).is_err() {
        m.enabled = old.0;
        m.alerts = old.1;
        m.selection = old.2;
        m.sync_devices = old.3;
        m.sync_configured = old.4;
        m.dirty = old.5;
        m.baseline_pending = old.6;
        m.sync_group = old.7;
    }
    let group_current = m.sync_group == group;
    let selection = m
        .selection
        .as_ref()
        .filter(|s| group.as_ref().is_some_and(|g| &s.group == g));
    let receiver_enabled = if group.is_some() {
        group_current
            && selection.is_some_and(|s| s.enabled && s.device_id == m.device_id)
            && m.enabled
    } else {
        selection.is_none() && m.enabled
    };
    let sync_devices = if group_current {
        m.sync_devices.clone()
    } else {
        BTreeMap::new()
    };
    Json(
        json!({"receiver_enabled":receiver_enabled,"receiver_selection":selection,"receiver_sync":{"status":if group.is_none() {"local_only"} else if !group_current || sync_devices.is_empty() {"pending"} else if sync_devices.values().all(|s| s == "synced") {"synced"} else {"partial"},"devices":sync_devices},"devices":m.devices.values().collect::<Vec<_>>(),"discovery_error":m.discovery_error}),
    )
}
pub async fn receiver(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
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
    let Some(token) = state.config.ai_token.as_deref().filter(|t| t.len() >= 32) else {
        let mut m = monitor().lock().unwrap();
        initialize(&mut m);
        let old = (
            m.enabled,
            m.alerts.clone(),
            m.dirty,
            m.selection.clone(),
            m.sync_devices.clone(),
            m.sync_configured,
            m.sync_group.clone(),
            m.baseline_pending.clone(),
        );
        m.selection = None;
        m.sync_devices.clear();
        m.alerts.clear();
        m.sync_configured = false;
        m.sync_group = None;
        m.enabled = enabled;
        if enabled {
            m.baseline_pending = m.baseline_sources.clone();
            let device_keys = m.devices.keys().cloned().collect::<Vec<_>>();
            m.baseline_pending.extend(device_keys);
            m.baseline_pending.insert("local".into());
        } else {
            m.baseline_pending.clear();
        }
        m.dirty |= old.0 != m.enabled
            || old.1 != m.alerts
            || old.3 != m.selection
            || old.4 != m.sync_devices
            || old.5 != m.sync_configured
            || old.7 != m.baseline_pending;
        if save(&mut m).is_err() {
            m.enabled = old.0;
            m.alerts = old.1;
            m.dirty = old.2;
            m.selection = old.3;
            m.sync_devices = old.4;
            m.sync_configured = old.5;
            m.sync_group = old.6;
            m.baseline_pending = old.7;
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "cannot save receiver setting",
            );
        }
        return (StatusCode::NO_CONTENT, "");
    };
    let mut m = monitor().lock().unwrap();
    initialize(&mut m);
    let old = (
        m.enabled,
        m.alerts.clone(),
        m.dirty,
        m.selection.clone(),
        m.baseline_pending.clone(),
        m.sync_devices.clone(),
        m.sync_configured,
        m.sync_group.clone(),
    );
    let group = group_id(token);
    if m.selection.as_ref().is_some_and(|s| s.group != group) {
        m.selection = None;
        m.enabled = false;
        m.alerts.clear();
        m.baseline_pending.clear();
        m.sync_devices.clear();
    }
    m.sync_configured = true;
    m.sync_group = Some(group.clone());
    if enabled {
        let counter = m
            .selection
            .as_ref()
            .map(|s| s.counter)
            .unwrap_or(0)
            .saturating_add(1);
        m.selection = Some(ReceiverSelection {
            device_id: m.device_id.clone(),
            display_name: state.config.display_name(),
            counter,
            operation_id: uuid::Uuid::new_v4().to_string(),
            enabled: true,
            group,
        });
        m.enabled = true;
        m.baseline_pending = m.baseline_sources.clone();
        let device_keys = m.devices.keys().cloned().collect::<Vec<_>>();
        m.baseline_pending.extend(device_keys);
        m.baseline_pending.insert("local".into());
        m.alerts.clear();
    } else {
        // A local release only wins when this device currently owns the declaration.
        if m.selection
            .as_ref()
            .is_some_and(|s| s.enabled && s.device_id == m.device_id)
        {
            let current = m.selection.as_ref().unwrap().clone();
            m.selection = Some(ReceiverSelection {
                counter: current.counter.saturating_add(1),
                operation_id: uuid::Uuid::new_v4().to_string(),
                enabled: false,
                ..current
            });
        }
        m.enabled = false;
        m.alerts.clear();
    }
    m.dirty = true;
    if save(&mut m).is_err() {
        m.enabled = old.0;
        m.alerts = old.1;
        m.dirty = old.2;
        m.selection = old.3;
        m.baseline_pending = old.4;
        m.sync_devices = old.5;
        m.sync_configured = old.6;
        m.sync_group = old.7;
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "cannot save receiver setting",
        );
    }
    (StatusCode::NO_CONTENT, "")
}

pub(crate) fn published_selection(token: &str) -> Option<ReceiverSelection> {
    let mut m = monitor().lock().ok()?;
    initialize(&mut m);
    let selection = m.selection.as_ref()?.clone();
    (selection.group == group_id(token)).then_some(selection)
}
fn merge_selection(m: &mut Monitor, remote: ReceiverSelection, group: &str) {
    if remote.group != group
        || uuid::Uuid::parse_str(&remote.device_id).is_err()
        || uuid::Uuid::parse_str(&remote.operation_id).is_err()
        || remote.display_name.len() > 128
        || remote.display_name.chars().any(char::is_control)
        || remote.counter > u64::MAX - 1
    {
        return;
    }
    if m.selection
        .as_ref()
        .is_none_or(|current| selection_order(current, &remote).is_lt())
    {
        let previous_enabled = m.enabled;
        m.selection = Some(remote.clone());
        let enabled = remote.enabled && remote.device_id == m.device_id;
        m.dirty = true;
        if m.enabled != enabled || !enabled && !m.alerts.is_empty() {
            m.enabled = enabled;
            m.alerts.clear();
            m.dirty = true;
            if enabled && !previous_enabled {
                m.baseline_pending = m.baseline_sources.clone();
                let device_keys = m.devices.keys().cloned().collect::<Vec<_>>();
                m.baseline_pending.extend(device_keys);
                m.baseline_pending.insert("local".into());
            }
        }
    }
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
fn receiver_active(m: &Monitor, config: &Config) -> bool {
    if !m.enabled {
        return false;
    }
    let group = config
        .ai_token
        .as_deref()
        .filter(|t| t.len() >= 32)
        .map(group_id);
    match group {
        Some(group) => {
            m.sync_group.as_deref() == Some(group.as_str())
                && m.selection
                    .as_ref()
                    .is_some_and(|s| s.group == group && s.enabled && s.device_id == m.device_id)
        }
        None => !m.sync_configured && m.sync_group.is_none() && m.selection.is_none(),
    }
}

pub fn take_alert(config: &Config) -> Option<String> {
    let mut m = monitor().lock().ok()?;
    // A restored queue may be read before the first async discovery finishes.
    // Validate the active configuration at consumption, not only on polling.
    if receiver_active(&m, config) {
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
    let first_feed = !m.baseline_sources.contains(&key);
    if first_feed && m.baseline_sources.len() < 1024 {
        m.baseline_sources.insert(key.clone());
    }
    let activation_baseline = m.baseline_pending.remove(&key);
    let suppress = first_feed || activation_baseline;
    m.dirty |= first_feed || activation_baseline;
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
        if !seen.contains(&id) && m.enabled && !suppress {
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
    device["error"] =
        json!("请检查版本、网络与共享认证令牌 / Check version, network and shared auth token");
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
        request = request.header("x-peercarry-ai-token", token);
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
async fn fetch_selection(
    client: &reqwest::Client,
    config: &Config,
    address: &str,
) -> anyhow::Result<Option<ReceiverSelection>> {
    let token = config
        .ai_token
        .as_deref()
        .filter(|t| t.len() >= 32)
        .ok_or_else(|| anyhow::anyhow!("AI token is not configured"))?;
    let mut request = client.get(format!("{address}/v1/ai/receiver-selection"));
    if let Some(shared) = &config.network.auth_token {
        request = request.header(protocol::TOKEN_HEADER, shared);
    }
    request = request.header("x-peercarry-ai-token", token);
    let mut response = request.send().await?.error_for_status()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        anyhow::ensure!(
            bytes.len() + chunk.len() <= 4096,
            "receiver selection too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
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
            if let Some(token) = config.ai_token.as_deref().filter(|t| t.len() >= 32) {
                let group = group_id(token);
                let group_changed = m.sync_group.as_deref() != Some(&group);
                let stale_selection = m.selection.as_ref().is_some_and(|s| s.group != group);
                if group_changed || stale_selection {
                    if stale_selection || (m.selection.is_none() && !m.sync_configured && m.enabled)
                    {
                        m.enabled = false;
                        m.alerts.clear();
                        m.selection = None;
                    }
                    if stale_selection {
                        m.selection = None;
                    }
                    if group_changed {
                        m.sync_devices.clear();
                        m.baseline_pending.clear();
                    }
                    m.sync_group = Some(group);
                    m.dirty = true;
                }
                m.sync_configured = true;
            } else {
                if m.selection.is_some() || m.sync_configured || m.sync_group.is_some() {
                    m.selection = None;
                    m.enabled = false;
                    m.alerts.clear();
                    m.baseline_pending.clear();
                    m.dirty = true;
                }
                m.sync_configured = false;
                m.sync_group = None;
                m.sync_devices.clear();
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
                let selection = if peer.online {
                    if let Some(address) = peer.addr_with_port(config.network.port) {
                        fetch_selection(client, config, &address).await
                    } else {
                        Err(anyhow::anyhow!("no address"))
                    }
                } else {
                    Err(anyhow::anyhow!("offline"))
                };
                (peer.node_id, peer.host, result, selection)
            }
        }))
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
        {
            let mut m = monitor().lock().unwrap();
            let receiver_before = (
                m.enabled,
                m.alerts.clone(),
                m.selection.clone(),
                m.baseline_pending.clone(),
                m.dirty,
                m.baseline_sources.clone(),
            );
            m.sync_devices.clear();
            for (key, device) in &mut m.devices {
                if key != "local" {
                    device["online"] = json!(false);
                }
            }
            let group = config
                .ai_token
                .as_deref()
                .filter(|t| t.len() >= 32)
                .map(group_id);
            for (_, _, _, selection) in &results {
                if let (Some(group), Ok(Some(selection))) = (&group, selection) {
                    merge_selection(&mut m, selection.clone(), group);
                }
            }
            match local {
                Ok(events) => record(&mut m, "local".into(), &config.display_name(), events),
                Err(_) => record_failure(&mut m, "local".into(), &config.display_name()),
            }
            for (node_id, host, result, selection) in results {
                match result {
                    Ok(events) => {
                        record(&mut m, node_id.clone(), &host, events);
                        match (&group, selection) {
                            (Some(_), Err(_)) => {
                                m.sync_devices.insert(node_id, "unsupported".into());
                            }
                            (Some(_), Ok(selection)) => {
                                let echoed = selection.as_ref() == m.selection.as_ref();
                                m.sync_devices.insert(
                                    node_id,
                                    if echoed { "synced" } else { "pending" }.into(),
                                );
                            }
                            (None, _) => {}
                        }
                    }
                    Err(_) => {
                        record_failure(&mut m, node_id.clone(), &host);
                        if group.is_some() {
                            m.sync_devices.insert(node_id, "offline".into());
                        }
                    }
                }
            }
            if let Err(error) = save(&mut m) {
                m.enabled = receiver_before.0;
                m.alerts = receiver_before.1;
                m.selection = receiver_before.2;
                m.baseline_pending = receiver_before.3;
                m.dirty = true;
                m.baseline_sources = receiver_before.5;
                m.sync_devices.clear();
                tracing::warn!("AI notification state save failed: {error}");
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn alert_consumption_rejects_stale_group_before_first_poll() {
        let mut config = Config {
            ai_token: Some("current-group-token-at-least-32-bytes".into()),
            ..Default::default()
        };
        let group = group_id(config.ai_token.as_deref().unwrap());
        let mut m = Monitor {
            enabled: true,
            device_id: "local".into(),
            sync_configured: true,
            sync_group: Some(group.clone()),
            selection: Some(ReceiverSelection {
                device_id: "local".into(),
                display_name: "local".into(),
                counter: 1,
                operation_id: "op".into(),
                enabled: true,
                group,
            }),
            ..Default::default()
        };
        assert!(receiver_active(&m, &config));
        config.ai_token = Some("another-group-token-at-least-32-bytes".into());
        assert!(!receiver_active(&m, &config));
        config.ai_token = None;
        assert!(!receiver_active(&m, &config));
        m.sync_configured = false;
        m.sync_group = None;
        m.selection = None;
        assert!(receiver_active(&m, &config));
    }
    use super::*;
    fn declaration(id: &str, counter: u64, enabled: bool, group: &str) -> ReceiverSelection {
        ReceiverSelection {
            device_id: uuid::Uuid::new_v4().to_string(),
            display_name: id.into(),
            counter,
            operation_id: id.into(),
            enabled,
            group: group.into(),
        }
    }
    #[test]
    fn concurrent_declarations_have_stable_lamport_tiebreak_and_restart_roundtrip() {
        let left = declaration("a", 7, true, "g");
        let right = declaration("b", 7, false, "g");
        assert_eq!(selection_order(&left, &right), std::cmp::Ordering::Less);
        let restored: ReceiverSelection =
            serde_json::from_slice(&serde_json::to_vec(&right).unwrap()).unwrap();
        assert_eq!(restored, right);
    }
    #[test]
    fn stale_group_declaration_is_ignored() {
        let mut m = Monitor::default();
        let foreign = declaration("z", 99, true, "old-group");
        merge_selection(&mut m, foreign, "current-group");
        assert!(m.selection.is_none());
        assert!(!m.enabled);
    }
    #[test]
    fn newly_selected_receiver_does_not_replay_the_first_feed() {
        let mut m = Monitor {
            enabled: true,
            baseline_pending: HashSet::from(["peer".to_owned()]),
            ..Default::default()
        };
        record(
            &mut m,
            "peer".into(),
            "peer",
            vec![json!({"id":"old","tool":"codex","session_id":"s","hook":"Stop","sequence":1})],
        );
        assert!(m.alerts.is_empty());
        assert!(m.baseline_sources.contains("peer"));
        assert!(!m.baseline_pending.contains("peer"));
    }
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
        // Establishing the source baseline must be persisted even for an
        // empty first feed; subsequent health changes alone need no write.
        assert!(m.dirty);
        m.dirty = false;
        record_failure(&mut m, "local".into(), "pc");
        assert_eq!(m.devices["local"]["online"], false);
        assert!(m.devices["local"]["error"].is_string());
        assert!(!m.dirty);
        record(&mut m, "local".into(), "pc", vec![]);
        assert_eq!(m.devices["local"]["online"], true);
        assert!(m.devices["local"].get("error").is_none());
        assert!(!m.dirty);
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

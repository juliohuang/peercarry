//! Optional bindings for local AI coding-tool hooks.
//!
//! The hook path is deliberately best-effort: a coding tool must never be
//! blocked because peercarry is stopped or its local daemon is unavailable.

use anyhow::{Context, Result};
use clap::Subcommand;
use serde_json::{json, Map, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

// Stable ownership marker for hooks configured before the PeerCarry rename.
const BINDING_ID: &str = "sync-clip-ai";
const MAX_STDIN: usize = 64 * 1024;
const MAX_SPOOL_FILES: usize = 256;
const MAX_SPOOL_BYTES: u64 = 8 * 1024 * 1024;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Subcommand)]
pub enum Args {
    /// Inspect whether supported AI tools are available on PATH.
    Scan,
    /// Install or remove the peercarry hook binding for one tool.
    Bind {
        #[arg(value_parser = ["codex", "zcode"])]
        tool: String,
        #[arg(long)]
        remove: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Receive one hook payload from an AI tool.
    Hook {
        #[arg(long, value_parser = ["codex", "zcode"])]
        tool: String,
        /// Internal marker used to identify entries owned by peercarry.
        #[arg(long, default_value = BINDING_ID, hide = true)]
        binding_id: String,
    },
}

pub async fn run(command: Args) -> Result<()> {
    match command {
        Args::Scan => scan(),
        Args::Bind {
            tool,
            remove,
            dry_run,
        } => bind(&tool, remove, dry_run),
        Args::Hook { tool, binding_id } => {
            // Hook failures are intentionally swallowed. stdout is part of the
            // hook protocol and must stay valid even when peercarry is broken.
            let result = hook(&tool, &binding_id).await;
            if tool == "codex" {
                println!("{{}}");
            }
            if let Err(error) = result {
                eprintln!("peercarry ai hook: {error:#}");
            }
            Ok(())
        }
    }
}

fn scan() -> Result<()> {
    let mut result = Map::new();
    let home = dirs::home_dir();
    for tool in ["codex", "zcode"] {
        let executable = find_on_path(tool).or_else(|| {
            if tool != "zcode" {
                return None;
            }
            let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;
            [
                local.join("Programs/ZCode/ZCode.exe"),
                local.join("ZCode/ZCode.exe"),
            ]
            .into_iter()
            .find(|p| p.is_file())
        });
        let path = config_path(tool)?;
        let configured = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|value| value.to_string().contains(BINDING_ID));
        let config_exists = home
            .as_ref()
            .map(|h| {
                if tool == "codex" {
                    h.join(".codex/hooks.json")
                } else {
                    h.join(".zcode/cli/config.json")
                }
            })
            .map(|p| p.is_file())
            .unwrap_or(false);
        result.insert(
            tool.to_string(),
            json!({ "available": executable.is_some() || config_exists, "config_exists": config_exists, "config_path":path, "configured":configured, "verified":false, "executable": executable.map(|p| p.display().to_string()) }),
        );
    }
    println!("{}", serde_json::to_string_pretty(&Value::Object(result))?);
    Ok(())
}

fn bind(tool: &str, remove: bool, dry_run: bool) -> Result<()> {
    let path = config_path(tool)?;
    let original = if path.exists() {
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?
    } else {
        "{}".to_string()
    };
    let mut document: Value = serde_json::from_str(&original)
        .with_context(|| format!("parse JSON in {}", path.display()))?;
    if !document.is_object() {
        anyhow::bail!("JSON root in {} must be an object", path.display());
    }
    let executable = hook_executable()?;
    let command = hook_command(tool, &executable)?;
    let changed = if tool == "codex" {
        edit_codex(&mut document, &command, remove)?
    } else {
        edit_zcode(&mut document, &command, &executable, remove)?
    };
    if !changed {
        println!("{}: no change", path.display());
        return Ok(());
    }
    if dry_run {
        println!(
            "{}: {} {}",
            path.display(),
            if remove { "remove" } else { "bind" },
            BINDING_ID
        );
        return Ok(());
    }
    atomic_write_json(&path, &document)?;
    println!(
        "{} {}",
        if remove { "removed" } else { "bound" },
        path.display()
    );
    Ok(())
}

fn config_path(tool: &str) -> Result<PathBuf> {
    let home = dirs::home_dir().context("cannot locate home directory")?;
    Ok(if tool == "codex" {
        home.join(".codex/hooks.json")
    } else {
        home.join(".zcode/cli/config.json")
    })
}

fn hook_executable() -> Result<PathBuf> {
    std::env::current_exe().context("locate peercarry executable")
}

fn hook_command(tool: &str, executable: &Path) -> Result<String> {
    let raw = executable.to_string_lossy();
    if raw.chars().any(|c| {
        c == '\r'
            || c == '\n'
            || c == '%'
            || c == '!'
            || c == '&'
            || c == '|'
            || c == '<'
            || c == '>'
    }) {
        anyhow::bail!("peercarry executable path contains unsafe shell characters");
    }
    let path = raw
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`");
    Ok(format!(
        "\"{path}\" ai hook --tool {tool} --binding-id {BINDING_ID}"
    ))
}

fn command_is_ours(value: &Value, command: &str) -> bool {
    value.get("type").and_then(Value::as_str) == Some("command")
        && value
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|configured| {
                configured == command
                    || configured
                        == command
                            .replace("peercarry.exe", "sclip.exe")
                            .replace("/peercarry\"", "/sclip\"")
            })
}

fn zcode_is_ours(value: &Value, command: &str) -> bool {
    command_is_ours(value, command)
        || (value.get("type").and_then(Value::as_str) == Some("process")
            && value
                .get("args")
                .and_then(Value::as_array)
                .map(|args| {
                    args == &vec![
                        json!("ai"),
                        json!("hook"),
                        json!("--tool"),
                        json!("zcode"),
                        json!("--binding-id"),
                        json!(BINDING_ID),
                    ]
                })
                .unwrap_or(false))
}

fn edit_codex(document: &mut Value, command: &str, remove: bool) -> Result<bool> {
    let root = document
        .as_object_mut()
        .context("JSON root must be an object")?;
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks
        .as_object_mut()
        .context("Codex hooks must be an object")?;
    let events = [
        "UserPromptSubmit",
        "Stop",
        "PermissionRequest",
        "PostToolUse",
        "Interrupt",
        "SessionEnd",
    ];
    let mut changed = false;
    for event in events {
        let list = hooks
            .entry(event)
            .or_insert_with(|| Value::Array(Vec::new()));
        let array = list
            .as_array_mut()
            .context("Codex hook event must be an array")?;
        let before = array.len();
        let had_ours = array.iter().any(|group| {
            group
                .get("hooks")
                .and_then(Value::as_array)
                .map(|hs| hs.iter().any(|h| command_is_ours(h, command)))
                .unwrap_or(false)
        });
        if remove {
            for group in array.iter_mut() {
                if let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                    handlers.retain(|entry| !command_is_ours(entry, command));
                }
            }
        }
        array.retain(|group| {
            group
                .get("hooks")
                .and_then(Value::as_array)
                .map(|v| !v.is_empty())
                .unwrap_or(true)
        });
        changed |= before != array.len() || (remove && had_ours);
        if !remove && !had_ours {
            array.push(json!({"hooks":[{"type":"command","command":command}]}));
            changed = true;
        }
        if remove && array.is_empty() {
            hooks.remove(event);
        }
    }
    Ok(changed)
}

fn edit_zcode(
    document: &mut Value,
    command: &str,
    executable: &Path,
    remove: bool,
) -> Result<bool> {
    let root = document
        .as_object_mut()
        .context("JSON root must be an object")?;
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks
        .as_object_mut()
        .context("ZCode hooks must be an object")?;
    let events = hooks.entry("events").or_insert_with(|| json!({}));
    let events = events
        .as_object_mut()
        .context("ZCode hook events must be an object")?;
    let supported = [
        "UserPromptSubmit",
        "Stop",
        "PermissionRequest",
        "PostToolUse",
        "PostToolUseFailure",
        "PostToolUseFailure",
    ];
    let mut changed = false;
    for event in supported {
        let list = events
            .entry(event)
            .or_insert_with(|| Value::Array(Vec::new()));
        let array = list
            .as_array_mut()
            .context("ZCode hook event must be an array")?;
        let before = array.len();
        let had_ours = array.iter().any(|group| {
            group
                .get("hooks")
                .and_then(Value::as_array)
                .map(|hs| hs.iter().any(|h| zcode_is_ours(h, command)))
                .unwrap_or(false)
        });
        if remove {
            for group in array.iter_mut() {
                if let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                    handlers.retain(|entry| !zcode_is_ours(entry, command));
                }
            }
        }
        array.retain(|group| {
            group
                .get("hooks")
                .and_then(Value::as_array)
                .map(|v| !v.is_empty())
                .unwrap_or(true)
        });
        changed |= before != array.len() || (remove && had_ours);
        if !remove && !had_ours {
            array.push(json!({"hooks":[{"type":"process","command":executable.to_string_lossy(),"args":["ai","hook","--tool","zcode","--binding-id",BINDING_ID]}]}));
            changed = true;
        } else if array.is_empty() {
            events.remove(event);
        }
    }
    if remove {
        if events.is_empty() {
            hooks.remove("events");
        }
    } else if hooks.get("enabled") != Some(&json!(true)) {
        hooks.insert("enabled".into(), json!(true));
        changed = true;
    }
    Ok(changed)
}

fn atomic_write_json(path: &Path, value: &Value) -> Result<()> {
    let parent = path.parent().context("config has no parent")?;
    std::fs::create_dir_all(parent)?;
    let bytes = serde_json::to_vec_pretty(value)?;
    let temp = path.with_extension("json.peercarry.tmp");
    let backup = path.with_extension("json.peercarry.bak");
    std::fs::write(&temp, &bytes)?;
    #[cfg(windows)]
    if path.exists() {
        let _ = std::fs::remove_file(&backup);
        std::fs::rename(path, &backup)?;
    }
    #[cfg(not(windows))]
    if path.exists() {
        let _ = std::fs::copy(path, &backup);
    }
    if let Err(error) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        #[cfg(windows)]
        if backup.exists() {
            let _ = std::fs::rename(&backup, path);
        }
        return Err(error.into());
    }
    Ok(())
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        for extension in [".exe", ".cmd", ".bat"] {
            let candidate = directory.join(format!("{name}{extension}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

async fn hook(tool: &str, binding_id: &str) -> Result<()> {
    if binding_id != BINDING_ID && binding_id != "peercarry-ai" {
        anyhow::bail!("unknown binding id");
    }
    let mut input = Vec::new();
    std::io::stdin()
        .take((MAX_STDIN + 1) as u64)
        .read_to_end(&mut input)?;
    if input.len() > MAX_STDIN {
        anyhow::bail!("hook input exceeds 64 KiB");
    }
    let payload: Value = serde_json::from_slice(&input).context("invalid hook JSON")?;
    let event = sanitize_event(tool, &payload)?;
    spool_event(&event, "pending")
}

fn sanitize_event(tool: &str, payload: &Value) -> Result<Value> {
    let object = payload
        .as_object()
        .context("hook payload must be an object")?;
    let session_id = object
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let cwd = object.get("cwd").and_then(Value::as_str).unwrap_or("");
    let event = object
        .get("hook_event_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    Ok(json!({
        "id": new_id(),
        "tool": tool,
        "session_id": session_id.chars().take(256).collect::<String>(),
        "project": Path::new(cwd).file_name().and_then(|v| v.to_str()).unwrap_or("").chars().take(128).collect::<String>(),
        "hook": event.chars().take(128).collect::<String>(),
    }))
}

fn new_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed) as u128;
    let value = now ^ ((std::process::id() as u128) << 64) ^ sequence;
    let hex = format!("{value:032x}");
    format!(
        "{}-{}-4{}-a{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        &hex[17..20],
        &hex[20..32]
    )
}

fn outbox_dir() -> PathBuf {
    peercarry_core::paths::data_dir().join("ai-outbox")
}

fn spool_event(event: &Value, reason: &str) -> Result<()> {
    let directory = outbox_dir();
    std::fs::create_dir_all(&directory)?;
    let mut files: Vec<_> = std::fs::read_dir(&directory)?
        .filter_map(|entry| entry.ok())
        .collect();
    files.sort_by_key(|entry| entry.metadata().and_then(|m| m.modified()).ok());
    while files.len() >= MAX_SPOOL_FILES
        || files
            .iter()
            .filter_map(|e| e.metadata().ok().map(|m| m.len()))
            .sum::<u64>()
            >= MAX_SPOOL_BYTES
    {
        if let Some(oldest) = files.first() {
            let _ = std::fs::remove_file(oldest.path());
        }
        files.remove(0);
    }
    let id = event.get("id").and_then(Value::as_str).unwrap_or("event");
    // The outbox file is itself a HookInput. Keeping the shape identical to
    // the daemon endpoint makes the worker drain path simple and auditable.
    let _ = reason;
    let pending = directory.join(format!("{id}.tmp"));
    std::fs::write(&pending, serde_json::to_vec(event)?)?;
    std::fs::rename(pending, directory.join(format!("{id}.json")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_binding_is_idempotent_and_removable() {
        let mut value = json!({"hooks":{"Stop":[{"hooks":[{"type":"command","command":"other"},{"type":"command","command":"ours"}]}]}});
        assert!(edit_codex(&mut value, "ours", false).unwrap());
        assert!(!edit_codex(&mut value, "ours", false).unwrap());
        assert!(edit_codex(&mut value, "ours", true).unwrap());
        assert_eq!(
            value["hooks"]["Stop"][0]["hooks"].as_array().unwrap().len(),
            1
        );
    }

    #[test]
    fn removing_only_our_handler_from_mixed_groups_reports_change() {
        let mut codex = json!({"hooks":{"Stop":[{"hooks":[{"type":"command","command":"other"},{"type":"command","command":"ours"}]}]}});
        assert!(edit_codex(&mut codex, "ours", true).unwrap());
        assert_eq!(codex["hooks"]["Stop"][0]["hooks"][0]["command"], "other");
        let mut zcode = json!({"hooks":{"enabled":true,"events":{"Stop":[{"hooks":[{"command":"other"},{"type":"process","command":"x","args":["ai","hook","--tool","zcode","--binding-id","sync-clip-ai"]}]}]}}});
        assert!(edit_zcode(&mut zcode, "ours", Path::new("x"), true).unwrap());
        assert_eq!(
            zcode["hooks"]["events"]["Stop"][0]["hooks"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn legacy_cli_hook_is_owned_after_rename() {
        let command = "\"C:/tools/peercarry.exe\" ai hook --tool codex --binding-id sync-clip-ai";
        assert!(command_is_ours(
            &json!({"type":"command", "command":command.replace("peercarry.exe", "sclip.exe")}),
            command
        ));
        assert!(!command_is_ours(
            &json!({"type":"command", "command":"other"}),
            command
        ));
    }

    #[test]
    fn malformed_binding_config_returns_error() {
        assert!(edit_codex(&mut json!({"hooks":{"Stop":{}}}), "ours", false).is_err());
    }

    #[test]
    fn zcode_remove_preserves_enabled_flag() {
        let mut value = json!({"hooks":{"enabled":true,"events":{"Stop":[{"hooks":[{"type":"process","command":"x","args":["ai","hook","--tool","zcode","--binding-id","sync-clip-ai"]}]}]}}});
        assert!(edit_zcode(&mut value, "ours", Path::new("x"), true).unwrap());
        assert_eq!(value["hooks"]["enabled"], true);
    }

    #[test]
    fn only_allowlisted_hook_fields_are_forwarded() {
        let value = sanitize_event(
            "codex",
            &json!({"session_id":"s","cwd":"C:/p","hook_event_name":"Stop","secret":"x"}),
        )
        .unwrap();
        assert!(value.get("secret").is_none());
        assert_eq!(value["project"], "p");
    }
}

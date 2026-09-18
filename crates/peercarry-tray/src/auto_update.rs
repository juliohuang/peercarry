//! Read-only update polling lives outside the clipboard command queue.
use peercarry_core::{config::Config, paths};
use std::time::{Duration, Instant};
use tao::event_loop::EventLoopProxy;

use super::{i18n, UserEvent};

fn due(enabled: bool, elapsed: Option<Duration>, hours: u64) -> bool {
    enabled && elapsed.is_none_or(|age| age >= Duration::from_secs(hours.clamp(1, 168) * 3600))
}

fn is_new(version: &str, notified: Option<&str>) -> bool {
    notified != Some(version)
}

pub async fn run(proxy: EventLoopProxy<UserEvent>) {
    let notice_path = paths::data_dir().join("update-notified-version");
    let mut notified = std::fs::read_to_string(&notice_path).ok();
    let mut last_check: Option<Instant> = None;
    loop {
        // Delay startup work; re-read preferences so opting out takes effect
        // without restarting. Never queue a download or installation here.
        tokio::time::sleep(Duration::from_secs(60)).await;
        let settings = match Config::load() {
            Ok(config) => config.updates,
            Err(error) => {
                tracing::debug!("Cannot read automatic update preferences: {error}");
                continue;
            }
        };
        if !due(
            settings.auto_check,
            last_check.map(|time| time.elapsed()),
            settings.check_interval_hours(),
        ) {
            continue;
        }
        last_check = Some(Instant::now());
        let Ok(url) = settings.manifest_url() else {
            continue;
        };
        let config = peercarry_updater::UpdateConfig {
            manifest_url: url.to_owned(),
            public_key: settings.public_key,
        };
        let release = match peercarry_updater::check(&config, env!("CARGO_PKG_VERSION")).await {
            Ok(Some(release)) => release,
            Ok(None) => continue,
            Err(error) => {
                tracing::debug!("Automatic update check failed; will retry later: {error}");
                continue;
            }
        };
        if !is_new(&release.version, notified.as_deref()) {
            continue;
        }
        // The user can disable checks while a network request is in progress.
        if !Config::load().is_ok_and(|config| config.updates.auto_check) {
            continue;
        }
        notified = Some(release.version.clone());
        if let Err(error) = std::fs::write(&notice_path, &release.version) {
            tracing::warn!("Cannot persist update notification: {error}");
        }
        let message = format!(
            "{} {}",
            i18n::choose("可更新至", "Update available:"),
            release.version
        );
        if proxy
            .send_event(UserEvent::Status(message.clone()))
            .is_err()
        {
            return;
        }
        // A dialog must not hold up clipboard commands or update polling.
        tokio::spawn(async move {
            rfd::AsyncMessageDialog::new()
                .set_title("PeerCarry")
                .set_description(format!("{message}\n\n{}", i18n::choose(
                    "在托盘菜单选择“更新并重启”，确认后安装。剪贴板与文件传输可继续使用。",
                    "Choose Update and restart in the tray menu, then confirm installation. Clipboard and file transfers remain available.")))
                .show().await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polling_respects_opt_out_and_interval_bounds() {
        assert!(due(true, None, 6));
        assert!(!due(false, None, 6));
        assert!(!due(true, Some(Duration::from_secs(21599)), 6));
        assert!(due(true, Some(Duration::from_secs(21600)), 6));
        assert!(!due(true, Some(Duration::from_secs(1)), 0));
        assert!(due(true, Some(Duration::from_secs(168 * 3600)), u64::MAX));
    }

    #[test]
    fn same_release_is_not_announced_after_reload() {
        assert!(is_new("0.4.0", None));
        assert!(!is_new("0.4.0", Some("0.4.0")));
        assert!(is_new("0.4.1", Some("0.4.0")));
    }
}

//! Menu and status strings follow the saved preference or the system UI language.
//!
//! The tray and its embedded windows share a language. CLI output is greppable and
//! stable across machines, so it stays in English and exposes `--json` for
//! anything that needs parsing.

pub use peercarry_core::locale::Language as Lang;

pub fn choose(zh: &'static str, en: &'static str) -> &'static str {
    match Lang::current() {
        Lang::Zh => zh,
        Lang::En => en,
    }
}

pub fn initialization_script() -> &'static str {
    initialization_script_for(Lang::current())
}

fn initialization_script_for(language: Lang) -> &'static str {
    match language {
        Lang::Zh => "window.__PEERCARRY_LANG__ = 'zh';",
        Lang::En => "window.__PEERCARRY_LANG__ = 'en';",
    }
}

/// Every user visible string in the tray app.
pub struct Ui {
    pub ready: &'static str,
    pub capture: &'static str,
    pub broadcast: &'static str,
    pub send_files: &'static str,
    pub pull: &'static str,
    pub pinned: &'static str,
    pub send: &'static str,
    pub open_download: &'static str,
    pub set_download: &'static str,
    pub timeline: &'static str,
    pub open_apps: &'static str,
    pub refresh: &'static str,
    pub quit: &'static str,
    pub empty: &'static str,
    pub recent_header: &'static str,
    pub devices_header: &'static str,
    pub quick_panel: &'static str,
    pub quick_hotkey_unavailable: &'static str,

    pub captured: &'static str,
    pub already_captured: &'static str,
    pub sent: &'static str,
    pub pulled: &'static str,
    pub pinned_ok: &'static str,
    pub unpinned_ok: &'static str,
    pub download_set: &'static str,
    pub launched: &'static str,
    pub gone: &'static str,

    pub err_prefix: &'static str,
    pub err_capture: &'static str,
    pub err_pull: &'static str,
    pub err_pin: &'static str,
    pub err_lookup: &'static str,
    pub err_refresh: &'static str,
    pub err_write: &'static str,
    pub err_send: &'static str,
    pub err_launch: &'static str,
    pub err_save: &'static str,
}

static EN: Ui = Ui {
    ready: "peercarry: ready",
    capture: "Capture Clipboard",
    broadcast: "Send to All Peers",
    send_files: "Send Files…",
    pull: "Pull to Clipboard",
    pinned: "Pinned",
    send: "Send to All Peers",
    open_download: "Open Download Folder",
    set_download: "Set Download Folder…",
    timeline: "Search Timeline…",
    open_apps: "Open App on Peer",
    refresh: "Refresh",
    quit: "Quit",
    empty: "(no entries - capture one first)",
    recent_header: "Recent · 10 min  (click = clipboard)",
    devices_header: "Devices · last 12 h",
    quick_panel: "Quick actions…",
    quick_hotkey_unavailable: "Quick panel hotkey unavailable; open it from the tray menu",

    captured: "captured {} · {}",
    already_captured: "already in history · {}",
    sent: "sent to {}/{} peers",
    pulled: "pulled {} from {}",
    pinned_ok: "pinned {}",
    unpinned_ok: "unpinned {}",
    download_set: "download folder: {}",
    launched: "launched {} on {}",
    gone: "entry is gone or its node is offline",

    err_prefix: "error: {}",
    err_capture: "capture failed: {}",
    err_pull: "pull failed: {}",
    err_pin: "pin failed: {}",
    err_lookup: "lookup failed: {}",
    err_refresh: "refresh failed: {}",
    err_write: "clipboard write failed: {}",
    err_send: "send failed: {}",
    err_launch: "launch failed: {}",
    err_save: "config save failed: {}",
};

static ZH: Ui = Ui {
    ready: "peercarry：就绪",
    capture: "抓取剪贴板",
    broadcast: "发给所有节点",
    send_files: "选择文件发送…",
    pull: "取回到本机剪贴板",
    pinned: "已固定",
    send: "发给所有节点",
    open_download: "打开接收文件夹",
    set_download: "设置接收文件夹…",
    timeline: "搜索时间线…",
    open_apps: "打开对端软件",
    refresh: "刷新",
    quit: "退出",
    empty: "（暂无条目 —— 先抓取一条）",
    recent_header: "最近 · 10 分钟（点击即复制）",
    devices_header: "按设备 · 12 小时内",
    quick_panel: "快捷操作…",
    quick_hotkey_unavailable: "快捷面板全局快捷键不可用；可从托盘菜单打开",

    captured: "已抓取 {} · {}",
    already_captured: "已在历史中 · {}",
    sent: "已发给 {}/{} 个节点",
    pulled: "已从 {} 取回 {}",
    pinned_ok: "已固定 {}",
    unpinned_ok: "已取消固定 {}",
    download_set: "接收文件夹：{}",
    launched: "已启动 {}（{}）",
    gone: "条目已不存在，或它的节点离线",

    err_prefix: "错误：{}",
    err_capture: "抓取失败：{}",
    err_pull: "取回失败：{}",
    err_pin: "固定失败：{}",
    err_lookup: "查找失败：{}",
    err_refresh: "刷新失败：{}",
    err_write: "写入剪贴板失败：{}",
    err_send: "发送失败：{}",
    err_launch: "启动失败：{}",
    err_save: "配置保存失败：{}",
};

/// Substitute `{}` left to right.
///
/// `format!` needs a literal template, which rules out swapping in a
/// translated one at runtime - and translations often need the holes in a
/// different order ("已从 {} 取回 {}"). So fill them here instead.
pub fn fill(template: &str, args: &[&str]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    let mut index = 0;
    while let Some(pos) = rest.find("{}") {
        out.push_str(&rest[..pos]);
        if let Some(arg) = args.get(index) {
            out.push_str(arg);
        }
        rest = &rest[pos + 2..];
        index += 1;
    }
    out.push_str(rest);
    out
}

/// Strings for the current local language preference.
pub fn ui() -> &'static Ui {
    match Lang::current() {
        Lang::Zh => &ZH,
        Lang::En => &EN,
    }
}

pub fn quick_panel_label() -> String {
    #[cfg(target_os = "macos")]
    let shortcut = "⌘⇧Space";
    #[cfg(not(target_os = "macos"))]
    let shortcut = "Ctrl+Shift+Space";
    format!("{}  {shortcut}", ui().quick_panel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialization_script_uses_the_shared_language_codes() {
        assert_eq!(
            initialization_script_for(Lang::Zh),
            "window.__PEERCARRY_LANG__ = 'zh';"
        );
        assert_eq!(
            initialization_script_for(Lang::En),
            "window.__PEERCARRY_LANG__ = 'en';"
        );
    }

    #[test]
    fn both_tables_are_filled_in() {
        // A missing translation would show up as an empty menu row.
        for field in [
            EN.ready,
            EN.capture,
            EN.send_files,
            EN.pull,
            EN.pinned,
            EN.send,
            EN.open_download,
            EN.set_download,
            EN.open_apps,
            EN.refresh,
            EN.quit,
            EN.recent_header,
            EN.devices_header,
            EN.quick_panel,
            EN.quick_hotkey_unavailable,
            EN.timeline,
        ] {
            assert!(!field.is_empty());
        }
        for field in [
            ZH.ready,
            ZH.capture,
            ZH.send_files,
            ZH.pull,
            ZH.pinned,
            ZH.send,
            ZH.open_download,
            ZH.set_download,
            ZH.open_apps,
            ZH.refresh,
            ZH.quit,
            ZH.recent_header,
            ZH.devices_header,
            ZH.quick_panel,
            ZH.quick_hotkey_unavailable,
            ZH.timeline,
        ] {
            assert!(!field.is_empty());
        }
    }
}

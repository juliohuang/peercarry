//! Menu bar / system tray application for peercarry.
//!
//! The app also runs the HTTP service, so starting it is all you need on
//! macOS and Windows.
//!
//! Threading: the GUI event loop owns the main thread (required by AppKit and
//! Win32). Every async operation runs on a Tokio runtime; when an entry is
//! pulled, the bytes are fetched on the runtime and only the final clipboard
//! write is handed back to the main thread.

// The tray owns its GUI event loop; Windows must not allocate a console for it.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod i18n;
mod update_install;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use chrono::{Duration, Utc};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tray_icon::menu::{
    CheckMenuItem, Icon as MenuIcon, IconMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem,
    Submenu,
};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

use peercarry_core::config::Config;
use peercarry_core::engine::{ClipboardWrite, Engine};
use peercarry_core::model::{Entry, EntryKind};
use peercarry_core::paths;
use peercarry_core::server;
use peercarry_core::store::Store;

/// How many entries the menu shows.
const MAX_MENU_ITEMS: usize = 15;
/// How many entries to fetch per machine (a few spare for grouped lists).
const FETCH_PER_PEER: usize = 12;
/// How recent an entry must be to appear in the flat click-to-paste list.
const RECENT_WINDOW_MIN: i64 = 10;
/// How long an entry stays listed under its device.
const DEVICE_WINDOW_H: i64 = 12;

// Menu ids are flat strings, so each action on an entry gets its own prefix.
const PULL_PREFIX: &str = "pull:";
const PIN_PREFIX: &str = "pin:";
const UNPIN_PREFIX: &str = "unpin:";
const SEND_PREFIX: &str = "send:";
/// Encodes both the target peer and the app: `launch:<host>:<app>`.
const LAUNCH_PREFIX: &str = "launch:";

// ------------------------------------------------------------------ plumbing

enum UserEvent {
    UpdateFinished,
    ExitForUpdate,
    PickFiles(bool),
    FilesDropped(Vec<PathBuf>),
    /// A tray menu item was clicked.
    Menu(MenuEvent),
    /// Fresh aggregated history.
    Entries(Vec<Entry>),
    /// Launchable apps advertised by online peers: `(host, app names)`.
    Apps(Vec<(String, Vec<String>)>),
    /// A decoded image preview, ready to be shown for one entry.
    Thumb(String, MenuIcon),
    /// Transient status line.
    Status(String),
    /// Error to surface in the menu.
    Error(String),
    /// Resolved clipboard payload, to be written on the main thread.
    Write(Box<ClipboardWrite>),
}

enum Command {
    Update(bool),
    Capture,
    Broadcast,
    /// Files picked in the native dialog, captured and broadcast.
    SendFiles(Vec<PathBuf>),
    Refresh,
    Apply(String),
    /// Flip the pinned flag on an entry, copying it here first if it lives
    /// on another machine.
    Pin(String, bool),
    /// Re-send an existing entry to every online peer.
    Send(String),
    /// Folder picked in the native dialog becomes the download folder.
    SetDownloadDir(PathBuf),
    /// Launch a registered app on the peer with this host name.
    LaunchApp(String, String),
}

struct AppState {
    update_busy: bool,
    entries: Vec<Entry>,
    /// Launchable apps per online peer.
    peer_apps: Vec<(String, Vec<String>)>,
    /// Decoded image previews, keyed by entry id.
    thumbs: HashMap<String, MenuIcon>,
    status: String,
    /// Signature of the menu as it is on screen right now. Refreshes mostly
    /// return identical data; rebuilding then would replace the shown menu
    /// and dismiss it, so identical renders are skipped.
    rendered: Option<String>,
    /// A rebuild was postponed because the menu was open; a timer retries
    /// until the menu is gone.
    dirty: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            update_busy: false,
            entries: Vec::new(),
            peer_apps: Vec::new(),
            thumbs: HashMap::new(),
            status: i18n::ui().ready.to_string(),
            rendered: None,
            dirty: false,
        }
    }
}

fn truncate(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!(
            "{}…",
            flat.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
}

/// Show a folder in the platform file manager. Fire-and-forget: Explorer
/// returns a non-zero status even on success, so the exit status is ignored.
fn open_folder(path: &Path) {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "linux")]
    let program = "xdg-open";
    #[cfg(target_os = "windows")]
    let program = "explorer";

    if let Err(e) = std::process::Command::new(program).arg(path).spawn() {
        tracing::error!("cannot open {}: {e}", path.display());
    }
}

/// Open a URL in the system browser. `explorer` on Windows avoids the
/// console window `cmd /c start` would flash. Fallback for when the native
/// app window cannot be created.
fn open_url(url: &str) {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "linux")]
    let program = "xdg-open";
    #[cfg(target_os = "windows")]
    let program = "explorer";

    if let Err(e) = std::process::Command::new(program).arg(url).spawn() {
        tracing::error!("cannot open {url}: {e}");
    }
}

/// The tray app's own window: timeline search and settings rendered by an
/// embedded web view against the local daemon - an app window, not a
/// browser tab.
///
/// A child web view does not track the window on its own: `fill_window`
/// must run at creation and on every resize.
fn fill_window(window: &tao::window::Window, webview: &wry::WebView) {
    let size = window.inner_size();
    let _ = webview.set_bounds(wry::Rect {
        position: wry::dpi::Position::Physical(wry::dpi::PhysicalPosition::new(0, 0)),
        size: wry::dpi::Size::Physical(wry::dpi::PhysicalSize::new(size.width, size.height)),
    });
}

fn build_app_window(
    target: &tao::event_loop::EventLoopWindowTarget<UserEvent>,
    port: u16,
    proxy: EventLoopProxy<UserEvent>,
) -> Result<(tao::window::Window, wry::WebView)> {
    let window = tao::window::WindowBuilder::new()
        .with_title("peercarry")
        .with_inner_size(tao::dpi::LogicalSize::new(940.0, 680.0))
        .with_min_inner_size(tao::dpi::LogicalSize::new(560.0, 420.0))
        .build(target)
        .map_err(|e| anyhow!("window: {e}"))?;
    let url = format!("http://127.0.0.1:{port}/");
    let ipc_url = url.clone();
    let navigation_url = url.clone();
    let drop_proxy = proxy.clone();
    let webview = wry::WebViewBuilder::new()
        .with_url(url)
        .with_navigation_handler(move |url| url == navigation_url)
        .with_ipc_handler(move |request| {
            if request.uri().to_string() != ipc_url {
                return;
            }
            match request.body().as_str() {
                "pick-files" => {
                    let _ = proxy.send_event(UserEvent::PickFiles(false));
                }
                "pick-folder" => {
                    let _ = proxy.send_event(UserEvent::PickFiles(true));
                }
                _ => {}
            }
        })
        .with_drag_drop_handler(move |event| {
            if let wry::DragDropEvent::Drop { paths, .. } = event {
                if !paths.is_empty() {
                    let _ = drop_proxy.send_event(UserEvent::FilesDropped(paths));
                }
            }
            true
        })
        .build_as_child(&window)
        .map_err(|e| anyhow!("webview: {e}"))?;
    fill_window(&window, &webview);
    Ok((window, webview))
}

// ----------------------------------------------------------------- tray icon

/// A 32x32 clipboard glyph drawn at runtime, so the binary needs no assets.
///
/// On macOS the icon is registered as a template, meaning the alpha channel
/// decides the shape and the system colours it for light/dark menu bars.
#[cfg(target_os = "macos")]
fn make_icon() -> Icon {
    const SIZE: u32 = 32;
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];

    let mut put = |x: u32, y: u32| {
        if x >= SIZE || y >= SIZE {
            return;
        }
        let idx = ((y * SIZE + x) * 4) as usize;
        rgba[idx] = 0;
        rgba[idx + 1] = 0;
        rgba[idx + 2] = 0;
        rgba[idx + 3] = 0xFF;
    };

    // Clip body outline.
    for x in 7..25 {
        put(x, 5);
        put(x, 28);
    }
    for y in 5..29 {
        put(7, y);
        put(24, y);
    }
    // Clip head.
    for x in 11..21 {
        for y in 2..6 {
            put(x, y);
        }
    }
    // Two content lines.
    for x in 11..21 {
        put(x, 13);
        put(x, 19);
    }

    Icon::from_rgba(rgba, SIZE, SIZE).expect("32x32 rgba is a valid icon")
}

/// GPT-generated color badge, embedded so installation needs only the binary.
#[cfg(not(target_os = "macos"))]
fn make_icon() -> Icon {
    let image = image::load_from_memory(include_bytes!("../assets/tray-icon.png"))
        .expect("embedded tray icon must be a valid PNG")
        .resize_exact(32, 32, image::imageops::FilterType::Lanczos3)
        .into_rgba8();
    Icon::from_rgba(image.into_raw(), 32, 32).expect("32x32 rgba is a valid icon")
}
// -------------------------------------------------------------- menu actions

/// What a click on an entry's menu item should do.
#[derive(Debug, PartialEq, Eq)]
enum EntryAction {
    Pull,
    /// `true` pins, `false` unpins.
    Pin(bool),
    Send,
}

/// Split a menu id into the action and the entry id it applies to.
///
/// The pin item's id has to encode the direction of the flip: muda reports
/// only an id, never the check state it landed on.
fn parse_entry_action(id: &str) -> Option<(EntryAction, &str)> {
    // Checked after "pin:": an id starting with "unpin:" does not match
    // "pin:", but keeping the order obvious avoids a surprise later.
    let parsed = if let Some(entry_id) = id.strip_prefix(PULL_PREFIX) {
        (EntryAction::Pull, entry_id)
    } else if let Some(entry_id) = id.strip_prefix(PIN_PREFIX) {
        (EntryAction::Pin(true), entry_id)
    } else if let Some(entry_id) = id.strip_prefix(UNPIN_PREFIX) {
        (EntryAction::Pin(false), entry_id)
    } else if let Some(entry_id) = id.strip_prefix(SEND_PREFIX) {
        (EntryAction::Send, entry_id)
    } else {
        return None;
    };

    // An action without an entry id would send the daemon chasing "".
    if parsed.1.is_empty() {
        return None;
    }
    Some(parsed)
}

/// The label of an entry's submenu: pinned marker, source, preview.
fn entry_label(entry: &Entry) -> String {
    let pin = if entry.pinned { "📌 " } else { "" };
    format!(
        "{}{} · {}",
        pin,
        truncate(&entry.origin.host, 14),
        truncate(&entry.preview, 34)
    )
}

/// Row label inside a device submenu: the host is already the submenu's
/// title, so only the pin marker and the preview remain.
fn entry_row_label(entry: &Entry) -> String {
    let pin = if entry.pinned { "📌 " } else { "" };
    format!("{}{}", pin, truncate(&entry.preview, 40))
}

/// Group entries by origin host, keeping the input's newest-first order:
/// devices appear by their most recent entry, entries newest-first inside.
fn group_by_device<'a>(
    entries: impl IntoIterator<Item = &'a Entry>,
) -> Vec<(String, Vec<&'a Entry>)> {
    let mut devices: Vec<(String, Vec<&'a Entry>)> = Vec::new();
    for entry in entries {
        match devices
            .iter_mut()
            .find(|(host, _)| host == &entry.origin.host)
        {
            Some((_, list)) => list.push(entry),
            None => devices.push((entry.origin.host.clone(), vec![entry])),
        }
    }
    devices
}

/// One entry as a submenu for the device lists: preview thumbnail on top,
/// then pull / pin / send. The flat recent list does without it.
fn entry_submenu(state: &AppState, entry: &Entry) -> Result<Submenu> {
    let pull = MenuItem::with_id(
        format!("{PULL_PREFIX}{}", entry.id),
        i18n::ui().pull,
        true,
        None,
    );
    let pin_item = CheckMenuItem::with_id(
        pin_item_id(entry),
        i18n::ui().pinned,
        true,
        entry.pinned,
        None,
    );
    let send = MenuItem::with_id(
        format!("{SEND_PREFIX}{}", entry.id),
        i18n::ui().send,
        true,
        None,
    );

    // The preview, when one has been decoded, sits above the actions. It is
    // not clickable, so it needs no prefix anyone dispatches on.
    let preview = state.thumbs.get(&entry.id).map(|icon| {
        IconMenuItem::with_id(
            format!("preview:{}", entry.id),
            "",
            true,
            Some(icon.clone()),
            None,
        )
    });

    let mut items: Vec<&dyn tray_icon::menu::IsMenuItem> = Vec::with_capacity(4);
    if let Some(preview) = &preview {
        items.push(preview);
    }
    items.push(&pull);
    items.push(&pin_item);
    items.push(&send);

    Ok(Submenu::with_items(&entry_row_label(entry), true, &items)?)
}

/// Menu id for the pin item, encoding the flip this click will perform.
fn pin_item_id(entry: &Entry) -> String {
    let prefix = if entry.pinned {
        UNPIN_PREFIX
    } else {
        PIN_PREFIX
    };
    format!("{}{}", prefix, entry.id)
}

// ----------------------------------------------------------------- rendering

/// Everything the rendered menu shows, flattened to one string. A refresh
/// that changes nothing produces the same signature, and the rebuild - which
/// would dismiss an open menu - is skipped.
fn render_signature(state: &AppState) -> String {
    use std::fmt::Write as _;
    let mut sig = String::with_capacity(512);
    let _ = write!(sig, "s:{}", state.status);
    let _ = write!(sig, "|updating:{}", state.update_busy);
    for e in &state.entries {
        let _ = write!(
            sig,
            "|e:{}{}{}{}",
            e.id, e.pinned as u8, e.origin.host, e.preview
        );
    }
    sig.push_str("|a");
    for (host, apps) in &state.peer_apps {
        let _ = write!(sig, "|a:{},{}", host, apps.join("+"));
    }
    let mut thumbs: Vec<&str> = state.thumbs.keys().map(String::as_str).collect();
    thumbs.sort_unstable();
    let _ = write!(sig, "|t:{}", thumbs.join(","));
    sig
}

/// True while a Win32 popup menu (window class `#32768`) is on screen. The
/// tray menu shows through `TrackPopupMenu`, whose modal loop keeps
/// dispatching posted messages - a rebuild during that window would destroy
/// the HMENU being displayed and slam the menu shut.
#[cfg(windows)]
fn popup_menu_open() -> bool {
    use windows::core::w;
    use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
    unsafe {
        FindWindowW(w!("#32768"), None)
            .map(|h| !h.is_invalid())
            .unwrap_or(false)
    }
}

#[cfg(not(windows))]
fn popup_menu_open() -> bool {
    // macOS tracks the menu modally on the main thread, so a re-entrant
    // render cannot happen in the first place.
    false
}

fn render(tray: &TrayIcon, state: &mut AppState) -> Result<()> {
    let signature = render_signature(state);
    if state.rendered.as_deref() == Some(signature.as_str()) {
        state.dirty = false;
        return Ok(());
    }
    if popup_menu_open() {
        // Menu is up; leave it alone. The event-loop timer re-runs render
        // until the menu closes, then this change lands.
        state.dirty = true;
        return Ok(());
    }

    let menu = Menu::new();

    menu.append(&MenuItem::with_id("status", &state.status, false, None))?;
    menu.append(&MenuItem::with_id(
        "version",
        format!("peercarry {}", env!("CARGO_PKG_VERSION")),
        false,
        None,
    ))?;
    menu.append(&MenuItem::with_id(
        "check-update",
        i18n::choose("检查更新", "Check for updates"),
        !state.update_busy,
        None,
    ))?;
    menu.append(&MenuItem::with_id(
        "install-update",
        i18n::choose("更新并重启", "Update and restart"),
        !state.update_busy,
        None,
    ))?;
    menu.append(&PredefinedMenuItem::separator())?;

    menu.append(&MenuItem::with_id(
        "capture",
        i18n::ui().capture,
        true,
        "CmdOrCtrl+Shift+C".parse().ok(),
    ))?;
    menu.append(&MenuItem::with_id(
        "broadcast",
        i18n::ui().broadcast,
        true,
        None,
    ))?;
    menu.append(&MenuItem::with_id(
        "send-files",
        i18n::ui().send_files,
        true,
        None,
    ))?;
    menu.append(&PredefinedMenuItem::separator())?;

    // Two lists, two jobs: the recent one trades context for speed (one
    // click pastes), the device one keeps 12 hours of context per machine.
    let now = Utc::now();

    let recent: Vec<&Entry> = state
        .entries
        .iter()
        .filter(|e| now.signed_duration_since(e.created_at) <= Duration::minutes(RECENT_WINDOW_MIN))
        .take(MAX_MENU_ITEMS)
        .collect();
    if !recent.is_empty() {
        menu.append(&MenuItem::with_id(
            "recent-header",
            i18n::ui().recent_header,
            false,
            None,
        ))?;
        for entry in recent {
            // Flat row: the click itself pulls, no detour through a submenu.
            menu.append(&MenuItem::with_id(
                format!("{PULL_PREFIX}{}", entry.id),
                entry_label(entry),
                true,
                None,
            ))?;
        }
        menu.append(&PredefinedMenuItem::separator())?;
    }

    let devices =
        group_by_device(state.entries.iter().filter(|e| {
            now.signed_duration_since(e.created_at) <= Duration::hours(DEVICE_WINDOW_H)
        }));
    menu.append(&MenuItem::with_id(
        "devices-header",
        i18n::ui().devices_header,
        false,
        None,
    ))?;
    if devices.is_empty() {
        menu.append(&MenuItem::with_id("empty", i18n::ui().empty, false, None))?;
    } else {
        for (host, entries) in &devices {
            let device_menu =
                Submenu::new(format!("{} ({})", truncate(host, 16), entries.len()), true);
            for entry in entries.iter().take(FETCH_PER_PEER) {
                device_menu.append(&entry_submenu(state, entry)?)?;
            }
            menu.append(&device_menu)?;
        }
    }

    // Peers that registered launchable apps get one flat row per app:
    // faster than drilling through peer submenus for the handful of
    // shortcuts people actually keep.
    if !state.peer_apps.is_empty() {
        let apps_menu = Submenu::new(i18n::ui().open_apps, true);
        let mut rows = 0usize;
        for (host, apps) in &state.peer_apps {
            for app in apps {
                if rows >= MAX_MENU_ITEMS {
                    break;
                }
                apps_menu.append(&MenuItem::with_id(
                    format!("{LAUNCH_PREFIX}{host}:{app}"),
                    format!("{host} · {app}"),
                    true,
                    None,
                ))?;
                rows += 1;
            }
        }
        menu.append(&apps_menu)?;
    }

    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&MenuItem::with_id(
        "open-download",
        i18n::ui().open_download,
        true,
        None,
    ))?;
    menu.append(&MenuItem::with_id(
        "set-download",
        i18n::ui().set_download,
        true,
        None,
    ))?;
    menu.append(&MenuItem::with_id(
        "timeline",
        i18n::ui().timeline,
        true,
        None,
    ))?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&MenuItem::with_id(
        "refresh",
        i18n::ui().refresh,
        true,
        None,
    ))?;
    menu.append(&MenuItem::with_id("quit", i18n::ui().quit, true, None))?;

    tray.set_menu(Some(Box::new(menu)));
    state.rendered = Some(signature);
    state.dirty = false;
    Ok(())
}

/// Render, and if the open popup menu forced a postponement, arm the event
/// loop to retry until the menu is gone. Every state-changing event goes
/// through this instead of calling `render` directly.
fn render_or_retry(tray: &TrayIcon, state: &mut AppState, control_flow: &mut ControlFlow) {
    if let Err(e) = render(tray, state) {
        tracing::error!("menu rebuild failed: {e}");
    }
    if state.dirty {
        *control_flow = ControlFlow::WaitUntil(
            std::time::Instant::now() + std::time::Duration::from_millis(250),
        );
    }
}

// -------------------------------------------------------------- command loop

/// Longest edge of the preview shown inside a menu.
const MENU_THUMB_PX: u32 = 32;

async fn refresh(engine: &Engine, proxy: &EventLoopProxy<UserEvent>, loaded: &mut HashSet<String>) {
    match engine.list_all(FETCH_PER_PEER).await {
        Ok(entries) => {
            let _ = proxy.send_event(UserEvent::Entries(entries.clone()));
            load_thumbs(engine, proxy, &entries, loaded).await;
        }
        Err(e) => {
            let _ = proxy.send_event(UserEvent::Error(i18n::fill(
                i18n::ui().err_refresh,
                &[&e.to_string()],
            )));
        }
    }
    // Which apps online peers offer; an old daemon answers with an empty
    // list, which simply hides the menu section.
    let apps = engine.client.remote_apps().await;
    let _ = proxy.send_event(UserEvent::Apps(apps));
}

/// Fetch and decode previews for the image entries on screen.
///
/// `loaded` keeps us from asking for the same bytes on every refresh: the
/// menu is rebuilt often and a preview, once shown, does not change.
async fn load_thumbs(
    engine: &Engine,
    proxy: &EventLoopProxy<UserEvent>,
    entries: &[Entry],
    loaded: &mut HashSet<String>,
) {
    // Only what the menu can show: entries inside the device window.
    let now = Utc::now();
    for entry in entries
        .iter()
        .filter(|e| now.signed_duration_since(e.created_at) <= Duration::hours(DEVICE_WINDOW_H))
    {
        if entry.kind != EntryKind::Image || entry.thumb.is_none() {
            continue;
        }
        if loaded.contains(&entry.id) {
            continue;
        }
        match engine.thumbnail_bytes(entry).await {
            Ok(Some(bytes)) => {
                if let Some(icon) = decode_icon(&bytes) {
                    loaded.insert(entry.id.clone());
                    let _ = proxy.send_event(UserEvent::Thumb(entry.id.clone(), icon));
                }
            }
            Ok(None) => {}
            Err(e) => tracing::debug!("no preview for {}: {e}", entry.id),
        }
    }
}

/// Decode a PNG preview into a menu icon, downscaled to keep menus compact.
fn decode_icon(png: &[u8]) -> Option<MenuIcon> {
    let image = image::load_from_memory(png).ok()?;
    let (width, height) = (image.width(), image.height());
    if width == 0 || height == 0 {
        return None;
    }
    let scale = (MENU_THUMB_PX as f64 / width.max(height) as f64).min(1.0);
    let tw = ((width as f64 * scale).round() as u32).max(1);
    let th = ((height as f64 * scale).round() as u32).max(1);

    let small = image::imageops::thumbnail(&image, tw, th);
    let rgba = small.into_raw();
    MenuIcon::from_rgba(rgba, tw, th).ok()
}

async fn update_application(
    engine: &Engine,
    install: bool,
    proxy: &EventLoopProxy<UserEvent>,
) -> Result<bool> {
    // Re-read only update settings; running service credentials/port stay unchanged.
    let settings = Config::load()?.updates;
    let config = peercarry_updater::UpdateConfig {
        manifest_url: settings
            .manifest_url()
            .map_err(anyhow::Error::msg)?
            .to_string(),
        public_key: settings.public_key,
    };
    let Some(release) = peercarry_updater::check(&config, env!("CARGO_PKG_VERSION")).await? else {
        let message = i18n::choose(
            "当前已是最新稳定版本",
            "Already on the latest stable version",
        );
        rfd::AsyncMessageDialog::new()
            .set_title("peercarry")
            .set_description(message)
            .show()
            .await;
        let _ = proxy.send_event(UserEvent::Status(message.into()));
        return Ok(false);
    };
    let description = format!(
        "{} → {}\n\n{}",
        env!("CARGO_PKG_VERSION"),
        release.version,
        truncate(&release.notes, 1500)
    );
    if !install {
        rfd::AsyncMessageDialog::new()
            .set_title(i18n::choose(
                "发现新版本：使用托盘的更新并重启",
                "New release: use Update and restart in the tray",
            ))
            .set_description(&description)
            .show()
            .await;
        let _ = proxy.send_event(UserEvent::Status(format!(
            "{} {}",
            i18n::choose("可更新至", "Update available:"),
            release.version
        )));
        return Ok(false);
    }
    let answer = rfd::AsyncMessageDialog::new()
        .set_title(i18n::choose(
            "下载更新并重启？",
            "Download update and restart?",
        ))
        .set_description(&description)
        .set_buttons(rfd::MessageButtons::OkCancel)
        .show()
        .await;
    if answer != rfd::MessageDialogResult::Ok {
        return Ok(false);
    }
    let _ = proxy.send_event(UserEvent::Status(
        i18n::choose("正在下载并验证更新…", "Downloading and verifying update…").into(),
    ));
    let staged =
        peercarry_updater::stage(&config, &release, &paths::data_dir().join("updates")).await?;
    if !peercarry_core::maintenance::try_quiesce() {
        let _ = std::fs::remove_file(&staged);
        return Err(anyhow!(
            "{}",
            i18n::choose(
                "当前有请求或传输正在进行，请完成后重试",
                "Requests or transfers are active; retry once they finish"
            )
        ));
    }
    let launch = update_install::launch(
        &staged,
        &release.version,
        engine.config.network.port,
        engine.config.network.auth_token.as_deref(),
    );
    if launch.is_err() {
        peercarry_core::maintenance::resume();
    }
    launch?;
    Ok(true)
}

async fn command_loop(
    engine: Arc<Engine>,
    mut rx: UnboundedReceiver<Command>,
    proxy: EventLoopProxy<UserEvent>,
) {
    // Entry ids whose preview has already been fetched.
    let mut loaded: HashSet<String> = HashSet::new();

    while let Some(command) = rx.recv().await {
        match command {
            Command::Update(install) => {
                let outcome = update_application(&engine, install, &proxy).await;
                match outcome {
                    Ok(true) => {
                        let _ = proxy.send_event(UserEvent::ExitForUpdate);
                        return;
                    }
                    Ok(false) => {}
                    Err(error) => {
                        peercarry_core::maintenance::resume();
                        let message =
                            format!("{}: {error:#}", i18n::choose("更新失败", "Update failed"));
                        rfd::AsyncMessageDialog::new()
                            .set_title("peercarry")
                            .set_description(&message)
                            .show()
                            .await;
                        let _ = proxy.send_event(UserEvent::Error(message));
                    }
                }
                let _ = proxy.send_event(UserEvent::UpdateFinished);
            }
            Command::Refresh => refresh(&engine, &proxy, &mut loaded).await,

            Command::Capture => {
                match engine.capture().await {
                    Ok(entry) => {
                        let message = if entry.deduped {
                            i18n::fill(
                                i18n::ui().already_captured,
                                &[&truncate(&entry.entry.preview, 28)],
                            )
                        } else {
                            i18n::fill(
                                i18n::ui().captured,
                                &[
                                    entry.entry.kind.as_str(),
                                    &truncate(&entry.entry.preview, 28),
                                ],
                            )
                        };
                        let _ = proxy.send_event(UserEvent::Status(message));
                    }
                    Err(e) => {
                        let _ = proxy.send_event(UserEvent::Error(i18n::fill(
                            i18n::ui().err_capture,
                            &[&e.to_string()],
                        )));
                    }
                }
                refresh(&engine, &proxy, &mut loaded).await;
            }

            Command::Broadcast => {
                match engine.capture().await {
                    Ok(outcome) => {
                        let results = engine.broadcast(&outcome.entry).await;
                        let ok = results.iter().filter(|(_, r)| r.is_ok()).count();
                        let total = results.len().to_string();
                        let done = ok.to_string();
                        let _ = proxy.send_event(UserEvent::Status(i18n::fill(
                            i18n::ui().sent,
                            &[&done, &total],
                        )));
                    }
                    Err(e) => {
                        let _ = proxy.send_event(UserEvent::Error(i18n::fill(
                            i18n::ui().err_capture,
                            &[&e.to_string()],
                        )));
                    }
                }
                refresh(&engine, &proxy, &mut loaded).await;
            }

            Command::SendFiles(paths) => {
                match engine.capture_files(&paths) {
                    Ok(outcome) => {
                        let results = engine.broadcast(&outcome.entry).await;
                        let ok = results.iter().filter(|(_, r)| r.is_ok()).count();
                        let total = results.len().to_string();
                        let done = ok.to_string();
                        let _ = proxy.send_event(UserEvent::Status(i18n::fill(
                            i18n::ui().sent,
                            &[&done, &total],
                        )));
                    }
                    Err(e) => {
                        let _ = proxy.send_event(UserEvent::Error(i18n::fill(
                            i18n::ui().err_send,
                            &[&e.to_string()],
                        )));
                    }
                }
                refresh(&engine, &proxy, &mut loaded).await;
            }

            Command::Pin(id, pinned) => {
                match engine.pin(&id, pinned).await {
                    Ok(Some(entry)) => {
                        let verb = if pinned {
                            i18n::ui().pinned_ok
                        } else {
                            i18n::ui().unpinned_ok
                        };
                        let _ = proxy.send_event(UserEvent::Status(i18n::fill(
                            verb,
                            &[&truncate(&entry.preview, 30)],
                        )));
                    }
                    Ok(None) => {
                        let _ = proxy.send_event(UserEvent::Error(i18n::ui().gone.to_string()));
                    }
                    Err(e) => {
                        let _ = proxy.send_event(UserEvent::Error(i18n::fill(
                            i18n::ui().err_pin,
                            &[&e.to_string()],
                        )));
                    }
                }
                refresh(&engine, &proxy, &mut loaded).await;
            }

            Command::Send(id) => {
                match engine.find(&id).await {
                    Ok(Some(entry)) => {
                        let results = engine.broadcast(&entry).await;
                        let ok = results.iter().filter(|(_, r)| r.is_ok()).count();
                        let total = results.len().to_string();
                        let done = ok.to_string();
                        let _ = proxy.send_event(UserEvent::Status(i18n::fill(
                            i18n::ui().sent,
                            &[&done, &total],
                        )));
                    }
                    Ok(None) => {
                        let _ = proxy.send_event(UserEvent::Error(i18n::ui().gone.to_string()));
                    }
                    Err(e) => {
                        let _ = proxy.send_event(UserEvent::Error(i18n::fill(
                            i18n::ui().err_lookup,
                            &[&e.to_string()],
                        )));
                    }
                }
                refresh(&engine, &proxy, &mut loaded).await;
            }

            Command::SetDownloadDir(path) => {
                // Persist to the config file and point the running engine at
                // the new folder, so the next pull uses it without a restart.
                let mut config = (*engine.config).clone();
                config.storage.download_dir = Some(path.clone());
                match config.save() {
                    Ok(()) => {
                        engine.set_download_dir(path.clone());
                        let _ = proxy.send_event(UserEvent::Status(i18n::fill(
                            i18n::ui().download_set,
                            &[&path.display().to_string()],
                        )));
                    }
                    Err(e) => {
                        let _ = proxy.send_event(UserEvent::Error(i18n::fill(
                            i18n::ui().err_save,
                            &[&e.to_string()],
                        )));
                    }
                }
            }

            Command::LaunchApp(host, app) => match engine.client.launch_on(&host, &app).await {
                Ok(_) => {
                    let _ = proxy.send_event(UserEvent::Status(i18n::fill(
                        i18n::ui().launched,
                        &[&app, &host],
                    )));
                }
                Err(e) => {
                    let _ = proxy.send_event(UserEvent::Error(i18n::fill(
                        i18n::ui().err_launch,
                        &[&e.to_string()],
                    )));
                }
            },

            Command::Apply(id) => {
                match engine.find(&id).await {
                    Ok(Some(entry)) => match engine.prepare(&entry).await {
                        Ok((payload, result)) => {
                            // Bytes are ready; hand the write to the main thread.
                            let _ = proxy.send_event(UserEvent::Write(Box::new(payload)));
                            let _ = proxy.send_event(UserEvent::Status(i18n::fill(
                                i18n::ui().pulled,
                                &[result.kind.as_str(), &entry.origin.host],
                            )));
                        }
                        Err(e) => {
                            let _ = proxy.send_event(UserEvent::Error(i18n::fill(
                                i18n::ui().err_pull,
                                &[&e.to_string()],
                            )));
                        }
                    },
                    Ok(None) => {
                        let _ = proxy.send_event(UserEvent::Error(i18n::ui().gone.to_string()));
                    }
                    Err(e) => {
                        let _ = proxy.send_event(UserEvent::Error(i18n::fill(
                            i18n::ui().err_lookup,
                            &[&e.to_string()],
                        )));
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------- main

fn main() -> Result<()> {
    if update_install::run_helper_if_requested()? {
        return Ok(());
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "peercarry_core=info,peercarry_tray=info".to_string()),
        )
        .with_target(false)
        .init();

    let config = Arc::new(Config::load()?);
    // Self-restart handoff (`POST /v1/actions/restart`): the fresh process
    // starts while the old one is still exiting, so hold here until the
    // port - and with it the store lock - is free.
    if std::env::var_os("PEERCARRY_RESTART").is_some() {
        server::wait_for_local_port(config.network.port, std::time::Duration::from_secs(10));
    }
    paths::ensure_layout()?;
    // Worth logging: the menu language surprises people otherwise.
    tracing::info!("menu language: {}", i18n::Lang::detect().code());

    let rt = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?,
    );

    // Resolve the bind address and origin before starting anything else, so
    // entries captured later carry a usable peer address.
    let (engine, srv_config, srv_store) = rt.block_on(async {
        let store = Arc::new(Store::open_default()?);
        let bind = server::bind_addr(&config).await;
        let origin = server::build_origin(&config, bind).await;
        let engine = Arc::new(Engine::new(config.clone(), store.clone(), origin)?);
        Ok::<_, anyhow::Error>((engine, config.clone(), store))
    })?;

    rt.spawn(async move {
        if let Err(e) = server::serve(srv_config, srv_store).await {
            tracing::error!("service stopped: {e}");
        }
    });

    #[allow(unused_mut)]
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        // Accessory keeps the app out of the Dock: it lives in the menu bar.
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
        event_loop.set_dock_visibility(false);
    }
    let proxy = event_loop.create_proxy();

    let menu_proxy = proxy.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let _ = menu_proxy.send_event(UserEvent::Menu(event));
    }));

    let (tx, rx): (UnboundedSender<Command>, UnboundedReceiver<Command>) =
        mpsc::unbounded_channel();
    // Clicking the icon itself refreshes, so the menu is current without
    // reaching for the Refresh item. Platforms render the menu as a snapshot
    // taken when it opens, so fresh content lands on the next open.
    let click_tx = tx.clone();
    let last_click = Arc::new(std::sync::atomic::AtomicU64::new(0));
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if !matches!(
            event,
            TrayIconEvent::Click { .. } | TrayIconEvent::DoubleClick { .. }
        ) {
            return;
        }
        // One gesture emits several events (down, up, double click); one
        // refresh per gesture is enough.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let previous = last_click.swap(now, std::sync::atomic::Ordering::Relaxed);
        if now.saturating_sub(previous) < 500 {
            return;
        }
        let _ = click_tx.send(Command::Refresh);
    }));
    // The event loop needs its own handles: engine goes to the command loop,
    // the runtime hosts the native file-dialog futures.
    let menu_engine = engine.clone();
    let ai_proxy = proxy.clone();
    rt.spawn(async move {
        loop {
            if let Some(message) = peercarry_core::ai_monitor::take_alert() {
                let _ = ai_proxy.send_event(UserEvent::Status(message.clone()));
                rfd::AsyncMessageDialog::new()
                    .set_title("peercarry · AI")
                    .set_description(&message)
                    .show()
                    .await;
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    });
    let dialog_rt = rt.clone();
    rt.spawn(command_loop(engine, rx, proxy.clone()));

    let mut tray_builder = TrayIconBuilder::new()
        .with_icon(make_icon())
        .with_tooltip("peercarry");
    #[cfg(target_os = "macos")]
    {
        tray_builder = tray_builder.with_icon_as_template(true);
    }
    let tray = tray_builder
        .build()
        .map_err(|e| anyhow!("cannot create tray icon: {e}"))?;

    let mut state = AppState::default();
    render(&tray, &mut state)?;
    let _ = tx.send(Command::Refresh);

    let loop_tx = tx.clone();
    // The daemon's local port, for the timeline page link.
    let timeline_port = config.network.port;
    // The app window with the timeline + settings page; one instance, the
    // menu item focuses it when it is already open.
    let mut app_ui: Option<(tao::window::Window, wry::WebView)> = None;
    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::NewEvents(StartCause::Init) => {
                let _ = loop_tx.send(Command::Refresh);
                // `PEERCARRY_WINDOW=1` opens the timeline window at startup -
                // a shortcut-friendly alternative to digging through the menu.
                if app_ui.is_none() && std::env::var_os("PEERCARRY_WINDOW").is_some() {
                    match build_app_window(target, timeline_port, proxy.clone()) {
                        Ok(ui) => app_ui = Some(ui),
                        Err(e) => tracing::error!("app window failed: {e}"),
                    }
                }
            }
            // A rebuild was postponed while the popup menu was up; the timer
            // armed by `render_or_retry` fires now and retries until the
            // menu is gone.
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) if state.dirty => {
                render_or_retry(&tray, &mut state, control_flow);
            }

            // Closing the app window just drops it; the tray keeps running.
            Event::WindowEvent {
                window_id,
                event: tao::event::WindowEvent::CloseRequested,
                ..
            } => {
                if app_ui.as_ref().is_some_and(|(w, _)| w.id() == window_id) {
                    app_ui = None;
                }
            }
            // The embedded web view does not track the window by itself.
            Event::WindowEvent {
                window_id,
                event: tao::event::WindowEvent::Resized(_),
                ..
            } => {
                if let Some((window, webview)) = &app_ui {
                    if window.id() == window_id {
                        fill_window(window, webview);
                    }
                }
            }

            Event::UserEvent(UserEvent::Menu(menu_event)) => {
                let id = menu_event.id().0.clone();
                if state.update_busy && !matches!(id.as_str(), "timeline" | "status" | "empty") {
                    return;
                }
                match id.as_str() {
                    "check-update" | "install-update" => {
                        if !state.update_busy {
                            state.update_busy = true;
                            state.status =
                                i18n::choose("正在检查更新…", "Checking for updates…").into();
                            let _ = loop_tx.send(Command::Update(id == "install-update"));
                            render_or_retry(&tray, &mut state, control_flow);
                        }
                    }
                    "capture" => {
                        let _ = loop_tx.send(Command::Capture);
                    }
                    "broadcast" => {
                        let _ = loop_tx.send(Command::Broadcast);
                    }
                    "send-files" => {
                        let tx = loop_tx.clone();
                        dialog_rt.spawn(async move {
                            if let Some(files) = rfd::AsyncFileDialog::new().pick_files().await {
                                let paths: Vec<PathBuf> =
                                    files.iter().map(|f| f.path().to_path_buf()).collect();
                                let _ = tx.send(Command::SendFiles(paths));
                            }
                        });
                    }
                    "open-download" => {
                        let dir = menu_engine.download_dir();
                        let _ = std::fs::create_dir_all(&dir);
                        open_folder(&dir);
                    }
                    "set-download" => {
                        let tx = loop_tx.clone();
                        dialog_rt.spawn(async move {
                            if let Some(folder) = rfd::AsyncFileDialog::new().pick_folder().await {
                                let _ =
                                    tx.send(Command::SetDownloadDir(folder.path().to_path_buf()));
                            }
                        });
                    }
                    // The app's own window with timeline + settings.
                    "timeline" => match &mut app_ui {
                        Some((window, _)) => window.set_focus(),
                        None => match build_app_window(target, timeline_port, proxy.clone()) {
                            Ok(ui) => app_ui = Some(ui),
                            Err(e) => {
                                tracing::error!("app window failed: {e}");
                                open_url(&format!("http://127.0.0.1:{timeline_port}/"));
                            }
                        },
                    },
                    "refresh" => {
                        let _ = loop_tx.send(Command::Refresh);
                    }
                    "quit" => *control_flow = ControlFlow::Exit,
                    "status" | "empty" => {}
                    other => {
                        if let Some((action, entry_id)) = parse_entry_action(other) {
                            let command = match action {
                                EntryAction::Pull => Command::Apply(entry_id.to_string()),
                                EntryAction::Pin(pinned) => {
                                    Command::Pin(entry_id.to_string(), pinned)
                                }
                                EntryAction::Send => Command::Send(entry_id.to_string()),
                            };
                            let _ = loop_tx.send(command);
                        } else if let Some(rest) = other.strip_prefix(LAUNCH_PREFIX) {
                            if let Some((host, app)) = rest.split_once(':') {
                                let _ = loop_tx
                                    .send(Command::LaunchApp(host.to_string(), app.to_string()));
                            }
                        }
                    }
                }
            }

            Event::UserEvent(UserEvent::Entries(entries)) => {
                state.entries = entries;
                render_or_retry(&tray, &mut state, control_flow);
            }

            Event::UserEvent(UserEvent::UpdateFinished) => {
                state.update_busy = false;
                render_or_retry(&tray, &mut state, control_flow);
            }
            Event::UserEvent(UserEvent::ExitForUpdate) => *control_flow = ControlFlow::Exit,

            Event::UserEvent(UserEvent::Apps(apps)) => {
                state.peer_apps = apps;
                render_or_retry(&tray, &mut state, control_flow);
            }

            Event::UserEvent(UserEvent::Thumb(id, icon)) => {
                state.thumbs.insert(id, icon);
                render_or_retry(&tray, &mut state, control_flow);
            }

            Event::UserEvent(UserEvent::PickFiles(folder)) => {
                if state.update_busy {
                    return;
                }
                let tx = loop_tx.clone();
                dialog_rt.spawn(async move {
                    let paths = if folder {
                        rfd::AsyncFileDialog::new()
                            .pick_folder()
                            .await
                            .map(|f| vec![f.path().to_path_buf()])
                    } else {
                        rfd::AsyncFileDialog::new().pick_files().await.map(|files| {
                            files.into_iter().map(|f| f.path().to_path_buf()).collect()
                        })
                    };
                    if let Some(paths) = paths {
                        let _ = tx.send(Command::SendFiles(paths));
                    }
                });
            }
            Event::UserEvent(UserEvent::FilesDropped(paths)) => {
                if state.update_busy {
                    return;
                }
                let _ = loop_tx.send(Command::SendFiles(paths));
            }
            Event::UserEvent(UserEvent::Status(text)) => {
                if let Some((_, webview)) = &app_ui {
                    let message = serde_json::to_string(&text).unwrap_or_default();
                    let _ = webview
                        .evaluate_script(&format!("window.nativeFileStatus({message}, false)"));
                }
                state.status = text;
                render_or_retry(&tray, &mut state, control_flow);
            }

            Event::UserEvent(UserEvent::Error(text)) => {
                if let Some((_, webview)) = &app_ui {
                    let message = serde_json::to_string(&text).unwrap_or_default();
                    let _ = webview
                        .evaluate_script(&format!("window.nativeFileStatus({message}, true)"));
                }
                tracing::error!("{text}");
                state.status = i18n::fill(i18n::ui().err_prefix, &[&truncate(&text, 44)]);
                render_or_retry(&tray, &mut state, control_flow);
            }

            Event::UserEvent(UserEvent::Write(payload)) => {
                // Main thread: the only place we touch the clipboard.
                if let Err(e) = Engine::write(*payload) {
                    tracing::error!("clipboard write failed: {e}");
                    state.status = i18n::fill(i18n::ui().err_write, &[&e.to_string()]);
                    render_or_retry(&tray, &mut state, control_flow);
                }
            }

            _ => {}
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageEncoder;
    use peercarry_core::model::{EntryKind, Origin};

    fn entry(id: &str, pinned: bool) -> Entry {
        Entry {
            id: id.to_string(),
            kind: EntryKind::Text,
            origin: Origin {
                host: "examplehostMacBook-Pro.local".to_string(),
                node_id: "node-1".to_string(),
                addr: "http://100.1.2.3:5199".to_string(),
                os: "macos".to_string(),
            },
            created_at: chrono::Utc::now(),
            text: None,
            blob: None,
            thumb: None,
            files: None,
            preview: "hello world".to_string(),
            pinned,
            size: 11,
            holder: None,
        }
    }

    #[test]
    fn parses_every_entry_action() {
        assert_eq!(
            parse_entry_action("pull:abc"),
            Some((EntryAction::Pull, "abc"))
        );
        assert_eq!(
            parse_entry_action("pin:abc"),
            Some((EntryAction::Pin(true), "abc"))
        );
        assert_eq!(
            parse_entry_action("unpin:abc"),
            Some((EntryAction::Pin(false), "abc"))
        );
        assert_eq!(
            parse_entry_action("send:abc"),
            Some((EntryAction::Send, "abc"))
        );
    }

    /// "unpin:" must not be mistaken for "pin:" - the two prefixes overlap
    /// at the start of the word, and a wrong match would pin when the user
    /// asked to unpin.
    #[test]
    fn unpin_is_not_read_as_pin() {
        let (action, id) = parse_entry_action("unpin:xyz").expect("unpin should parse");
        assert_eq!(action, EntryAction::Pin(false));
        assert_eq!(id, "xyz");
    }

    #[test]
    fn ignores_ids_that_are_not_entry_actions() {
        for id in [
            "capture", "refresh", "quit", "status", "", "pin:", "PULL:abc",
        ] {
            assert_eq!(parse_entry_action(id), None, "{id} should not parse");
        }
    }

    fn test_png() -> Vec<u8> {
        let mut png = Vec::new();
        let img = image::RgbaImage::from_pixel(400, 200, image::Rgba([200, 30, 30, 255]));
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&img, 400, 200, image::ExtendedColorType::Rgba8)
            .expect("encoding a test png");
        png
    }

    /// Identical data must produce an identical signature - that is what
    /// keeps a refresh from rebuilding (and dismissing) an open menu.
    #[test]
    fn render_signature_ignores_equal_states() {
        let mut a = AppState::default();
        let mut b = AppState::default();
        a.entries = vec![entry("x", false)];
        b.entries = vec![entry("x", false)];
        assert_eq!(render_signature(&a), render_signature(&b));

        // Any visible change must alter the signature.
        b.entries = vec![entry("x", true)];
        assert_ne!(render_signature(&a), render_signature(&b));
        let mut c = AppState::default();
        c.entries = vec![entry("x", false)];
        c.status = "pulled text".to_string();
        assert_ne!(render_signature(&a), render_signature(&c));
        let mut d = AppState::default();
        d.entries = vec![entry("x", false)];
        d.thumbs
            .insert("x".to_string(), decode_icon(&test_png()).unwrap());
        assert_ne!(render_signature(&a), render_signature(&d));
    }

    #[test]
    fn pin_id_flips_with_state() {
        assert_eq!(pin_item_id(&entry("abc", false)), "pin:abc");
        assert_eq!(pin_item_id(&entry("abc", true)), "unpin:abc");
    }

    /// Devices appear by their most recent entry; entries stay newest-first
    /// inside their group. Both come for free from a newest-first input.
    #[test]
    fn groups_entries_by_device_in_activity_order() {
        let base = chrono::Utc::now();
        let mut mac_new = entry("a1", false);
        mac_new.origin.host = "mac".to_string();
        mac_new.created_at = base - chrono::Duration::hours(1);
        let mut legion = entry("b1", false);
        legion.origin.host = "legion".to_string();
        legion.created_at = base - chrono::Duration::hours(2);
        let mut mac_old = entry("a2", false);
        mac_old.origin.host = "mac".to_string();
        mac_old.created_at = base - chrono::Duration::hours(3);

        let groups = group_by_device([&mac_new, &legion, &mac_old]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, "mac");
        assert_eq!(groups[0].1.len(), 2);
        assert_eq!(groups[0].1[0].id, "a1");
        assert_eq!(groups[0].1[1].id, "a2");
        assert_eq!(groups[1].0, "legion");
    }

    /// The preview has to survive PNG decode and the downscale to menu size;
    /// a broken thumbnail should be dropped rather than break the menu.
    #[test]
    fn decodes_a_png_preview() {
        assert!(decode_icon(&test_png()).is_some());
        assert!(decode_icon(b"definitely not a png").is_none());
    }

    #[test]
    fn label_marks_pinned_and_truncates_host() {
        let plain = entry("abc", false);
        // Host is truncated to 14 columns, ellipsis included.
        assert_eq!(entry_label(&plain), "examplehostMa… · hello world");

        let pinned = entry("abc", true);
        assert!(entry_label(&pinned).starts_with("📌 "));
    }
}

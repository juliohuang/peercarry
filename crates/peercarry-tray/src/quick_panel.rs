//! Compact quick actions window. The page is served by the local daemon.

use anyhow::{anyhow, Result};
use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};

use crate::UserEvent;

pub fn build(
    target: &EventLoopWindowTarget<UserEvent>,
    port: u16,
    proxy: EventLoopProxy<UserEvent>,
) -> Result<(tao::window::Window, wry::WebView)> {
    let window = tao::window::WindowBuilder::new()
        .with_title("PeerCarry")
        .with_inner_size(tao::dpi::LogicalSize::new(390.0, 590.0))
        .with_min_inner_size(tao::dpi::LogicalSize::new(340.0, 420.0))
        .build(target)
        .map_err(|e| anyhow!("quick panel window: {e}"))?;
    let url = format!("http://127.0.0.1:{port}/quick");
    let allowed_url = url.clone();
    let ipc_url = url.clone();
    let webview = wry::WebViewBuilder::new()
        .with_initialization_script(crate::i18n::initialization_script())
        .with_url(url)
        .with_navigation_handler(move |candidate| candidate == allowed_url)
        .with_ipc_handler(move |request| {
            if request.uri().to_string() != ipc_url {
                return;
            }
            if request.body().as_str() == "quick-close" {
                let _ = proxy.send_event(UserEvent::CloseQuickPanel);
            }
        })
        .build_as_child(&window)
        .map_err(|e| anyhow!("quick panel webview: {e}"))?;
    let size = window.inner_size();
    let _ = webview.set_bounds(wry::Rect {
        position: wry::dpi::Position::Physical(wry::dpi::PhysicalPosition::new(0, 0)),
        size: wry::dpi::Size::Physical(wry::dpi::PhysicalSize::new(size.width, size.height)),
    });
    Ok((window, webview))
}

#![cfg(target_os = "windows")]

use peercarry_automation::windows::WindowsBackend;
use peercarry_automation::{Backend, Selector};

fn selector(role: &str) -> Selector {
    Selector {
        role: role.into(),
        name: Some("test".into()),
        automation_id: None,
        region: None,
    }
}

#[test]
fn input_rejects_control_characters_before_target_access() {
    let mut backend = WindowsBackend::new().expect("COM/UIA should initialize");
    let error = backend
        .input_text("line\nbreak")
        .expect_err("newline must never reach UIA");
    assert!(error.to_string().contains("control characters"));
}

#[test]
fn button_click_is_refused_before_target_access() {
    let mut backend = WindowsBackend::new().expect("COM/UIA should initialize");
    let error = backend
        .click(&selector("button"))
        .expect_err("buttons are never invoked");
    assert!(error.to_string().contains("SelectionItem"));
}

#[test]
fn paste_is_explicitly_unsupported_and_does_not_touch_clipboard() {
    let mut backend = WindowsBackend::new().expect("COM/UIA should initialize");
    let error = backend
        .paste("safe text")
        .expect_err("P1 paste is deliberately unsupported");
    assert!(error
        .to_string()
        .contains("clipboard paste is not implemented"));
}

#[test]
fn expand_rejects_button_before_target_access() {
    let mut backend = WindowsBackend::new().expect("COM/UIA should initialize");
    let error = backend
        .expand(&selector("button"))
        .expect_err("only combo/menu controls may expand");
    assert!(error.to_string().contains("combo_box") || error.to_string().contains("menu"));
}

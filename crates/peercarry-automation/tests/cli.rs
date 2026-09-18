use std::{path::PathBuf, process::Command};

fn example() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/automation/focus-prompt.json")
}

#[test]
fn documented_single_path_command_validates_without_desktop() {
    let output = Command::new(env!("CARGO_BIN_EXE_peercarry-automation"))
        .arg(example())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("valid"));
}

#[test]
fn unknown_cli_arguments_are_not_silently_ignored() {
    let output = Command::new(env!("CARGO_BIN_EXE_peercarry-automation"))
        .arg(example())
        .arg("--exectue")
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn missing_inspect_target_is_rejected() {
    let output = Command::new(env!("CARGO_BIN_EXE_peercarry-automation"))
        .args(["inspect", "--exe", "ZCode.exe"])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

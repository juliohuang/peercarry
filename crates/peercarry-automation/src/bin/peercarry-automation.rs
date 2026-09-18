//! Development helper. Execution requires an explicit --execute flag.
use peercarry_automation::parse_and_validate;
use std::{env, fs, process::ExitCode};

fn main() -> ExitCode {
    let mut args = env::args_os();
    let _ = args.next();
    let first = args.next();
    if first.as_deref() == Some(std::ffi::OsStr::new("inspect")) {
        #[cfg(target_os = "windows")]
        {
            let mut exe = String::new();
            let mut title = String::new();
            while let Some(arg) = args.next() {
                match arg.to_string_lossy().as_ref() {
                    "--exe" => {
                        exe = args
                            .next()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned()
                    }
                    "--title" => {
                        title = args
                            .next()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned()
                    }
                    _ => {
                        eprintln!("unknown inspect argument");
                        return ExitCode::from(2);
                    }
                }
            }
            if exe.is_empty() || title.is_empty() {
                eprintln!(
                    "usage: peercarry-automation inspect --exe <exe> --title <exact-window-title>"
                );
                return ExitCode::from(2);
            }
            return match peercarry_automation::windows::inspect(&exe, &title) {
                Ok(items) => {
                    for i in items {
                        if matches!(
                            i.role.as_str(),
                            "button" | "edit" | "combo_box" | "menu" | "menu_item" | "list_item"
                        ) {
                            println!(
                                "role={} name={:?} automation_id={:?}",
                                i.role, i.name, i.automation_id
                            );
                        }
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("inspect failed: {e}");
                    ExitCode::from(1)
                }
            };
        }
        #[cfg(not(target_os = "windows"))]
        {
            eprintln!("inspect is supported on Windows only");
            return ExitCode::from(3);
        }
    }
    let Some(path) = first else {
        eprintln!("usage: peercarry-automation <flow.json>");
        return ExitCode::from(2);
    };
    let execute = match args.next() {
        None => false,
        Some(flag) if flag == "--execute" => true,
        Some(_) => {
            eprintln!("expected --execute or no additional argument");
            return ExitCode::from(2);
        }
    };
    if args.next().is_some() {
        eprintln!("unexpected arguments after flow path");
        return ExitCode::from(2);
    }
    let text = match fs::read_to_string(path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("read failed: {e}");
            return ExitCode::from(2);
        }
    };
    match parse_and_validate(&text) {
        Ok(plan) => {
            if execute {
                #[cfg(target_os = "windows")]
                {
                    use peercarry_automation::{windows::WindowsBackend, Action, Runner};
                    // Reject unsupported mutations before even focusing a window.
                    if plan.steps.iter().any(|s| matches!(&s.action, Action::Paste { .. } | Action::Submit { .. })
                        || matches!(&s.action, Action::Click { selector } if !matches!(selector.role.as_str(), "list_item" | "menu_item"))) {
                        eprintln!("P1 execution refuses paste, submit, and arbitrary button clicks");
                        return ExitCode::from(1);
                    }
                    let result = WindowsBackend::new()
                        .and_then(|backend| Runner::new(backend, false).run(&plan, true));
                    return match result {
                        Ok(()) => {
                            println!(
                                "flow completed: {} steps verified; no submit action",
                                plan.steps.len()
                            );
                            ExitCode::SUCCESS
                        }
                        Err(e) => {
                            eprintln!("execution stopped: {e}");
                            ExitCode::from(1)
                        }
                    };
                }
                #[cfg(not(target_os = "windows"))]
                {
                    eprintln!("native execution is supported on Windows only");
                    return ExitCode::from(3);
                }
            }
            println!(
                "valid flow: {} ({} steps; dry-run only)",
                plan.name,
                plan.steps.len()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("invalid flow: {e}");
            ExitCode::from(1)
        }
    }
}

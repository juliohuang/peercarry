//! Deterministic P0 simulation. No browser, phone, screen or real credentials.
use peercarry_login_assist::{Coordinator, Operation, Rect, Scope, Target};
use std::collections::HashSet;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = Coordinator::default();
    let target = Target {
        device: "fixture-mac".into(),
        browser: "fixture-chrome".into(),
        tab: 1,
        document: "fixture-document".into(),
        frame: 0,
        origin: "http://127.0.0.1:5188".into(),
    };
    let id = host.request(
        target.clone(),
        "mock-ai".into(),
        "mock-phone".into(),
        0,
        1000,
    )?;
    host.pause_ack(id, "mock-ai", 1, 1)?;
    let rect = Rect {
        x: 0,
        y: 0,
        width: 320,
        height: 200,
    };
    let epoch = host.approve(
        id,
        "mock-phone",
        &target,
        Scope {
            view: rect,
            clicks: Some(rect),
            fields: HashSet::from(["password".into()]),
        },
        2,
    )?;
    host.authorize(
        id,
        "mock-phone",
        &target,
        epoch,
        1,
        Operation::Fill("password"),
        3,
    )?;
    let epoch = host.hand_back(id, "mock-phone", epoch, 4)?;
    host.revoked_ack(id, epoch, 5)?;
    host.resume_ack(id, "mock-ai", epoch, true, 6)?;
    println!("SIMULATED ONLY: {:?}", host.status(id, "mock-ai", 7)?);
    Ok(())
}

use peercarry_core::maintenance::*;

#[test]
fn active_stream_prevents_shutdown_and_quiescence_blocks_new_work() {
    let operation = enter().unwrap();
    assert!(!try_quiesce());
    drop(operation);
    assert!(try_quiesce());
    assert!(enter().is_none());
    resume();
    assert!(enter().is_some());
}

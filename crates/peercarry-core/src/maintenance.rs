//! Coordinates update shutdown with active HTTP bodies and detached upload IO.
use std::sync::Mutex;

static STATE: Mutex<(bool, usize)> = Mutex::new((false, 0));
pub struct Activity;
pub fn enter() -> Option<Activity> {
    let mut state = STATE.lock().ok()?;
    if state.0 {
        return None;
    }
    state.1 += 1;
    Some(Activity)
}
impl Drop for Activity {
    fn drop(&mut self) {
        if let Ok(mut state) = STATE.lock() {
            state.1 = state.1.saturating_sub(1);
        }
    }
}
pub fn try_quiesce() -> bool {
    let Ok(mut state) = STATE.lock() else {
        return false;
    };
    if state.0 || state.1 != 0 {
        return false;
    }
    state.0 = true;
    true
}
pub fn resume() {
    if let Ok(mut state) = STATE.lock() {
        state.0 = false;
    }
}

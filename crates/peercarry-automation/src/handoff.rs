//! Cooperative pause barrier for this runner's synchronous backend calls only.
//! It cannot pause unrelated processes or an external AI platform.
use crate::{AutomationError, StopToken};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct State {
    paused: bool,
    active: usize,
    lease: bool,
}
#[derive(Clone, Default)]
pub struct PauseController(Arc<(Mutex<State>, Condvar)>);
pub struct PauseLease {
    controller: PauseController,
}
pub(crate) struct OperationGuard(PauseController);

impl PauseController {
    pub(crate) fn enter(&self, stop: &StopToken) -> Result<OperationGuard, AutomationError> {
        let (lock, changed) = &*self.0;
        let mut state = lock.lock().map_err(|_| AutomationError::Stopped)?;
        while state.paused {
            if stop.stopped() {
                return Err(AutomationError::Stopped);
            }
            state = changed
                .wait_timeout(state, Duration::from_millis(50))
                .map_err(|_| AutomationError::Stopped)?
                .0;
        }
        if stop.stopped() {
            return Err(AutomationError::Stopped);
        }
        state.active += 1;
        Ok(OperationGuard(self.clone()))
    }

    /// Returns only after all reads/inputs currently inside this runner finish.
    /// Timeout and dropped leases remain paused; no automatic resume is possible.
    pub fn pause(&self, timeout: Duration) -> Result<PauseLease, AutomationError> {
        let (lock, changed) = &*self.0;
        let mut state = lock.lock().map_err(|_| AutomationError::Stopped)?;
        if state.lease || state.paused {
            return Err(AutomationError::Stopped);
        }
        state.paused = true;
        let start = Instant::now();
        while state.active > 0 {
            let remaining = timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Err(AutomationError::Timeout(timeout.as_millis() as u64));
            }
            state = changed
                .wait_timeout(state, remaining)
                .map_err(|_| AutomationError::Stopped)?
                .0;
        }
        state.lease = true;
        Ok(PauseLease {
            controller: self.clone(),
        })
    }

    /// Trusted local recovery after a timed-out pause or lost handoff owner.
    /// Recovery still returns a lease: the caller must explicitly resume it.
    pub fn reconcile(&self) -> Result<PauseLease, AutomationError> {
        let mut state = self.0 .0.lock().map_err(|_| AutomationError::Stopped)?;
        if !state.paused || state.active != 0 || state.lease {
            return Err(AutomationError::Stopped);
        }
        state.lease = true;
        Ok(PauseLease {
            controller: self.clone(),
        })
    }
}
impl PauseLease {
    /// Call only after private channels are revoked and queues discarded.
    pub fn resume(self) -> Result<(), AutomationError> {
        let (lock, changed) = &*self.controller.0;
        let mut state = lock.lock().map_err(|_| AutomationError::Stopped)?;
        state.paused = false;
        changed.notify_all();
        Ok(())
    }
}
impl Drop for PauseLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.controller.0 .0.lock() {
            state.lease = false;
        }
    }
}
impl Drop for OperationGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0 .0 .0.lock() {
            state.active -= 1;
            self.0 .0 .1.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pause_ack_waits_for_inflight_operations_and_resume_is_explicit() {
        let gate = PauseController::default();
        let operation = gate.enter(&StopToken::default()).unwrap();
        let other = gate.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            tx.send(other.pause(Duration::from_secs(2)).unwrap())
                .unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(30)).is_err());
        drop(operation);
        let lease = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let next = gate.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _guard = next.enter(&StopToken::default()).unwrap();
            tx.send(()).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(30)).is_err());
        lease.resume().unwrap();
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        thread.join().unwrap();
        worker.join().unwrap();
    }
    #[test]
    fn dropped_lease_or_timeout_does_not_resume() {
        let gate = PauseController::default();
        drop(gate.pause(Duration::from_secs(1)).unwrap());
        assert!(gate.pause(Duration::from_secs(1)).is_err());
        gate.reconcile().unwrap().resume().unwrap();
        let operation = gate.enter(&StopToken::default()).unwrap();
        assert!(gate.pause(Duration::from_millis(1)).is_err());
        assert!(gate.reconcile().is_err());
        drop(operation);
        gate.reconcile().unwrap().resume().unwrap();
        assert!(gate.enter(&StopToken::default()).is_ok());
    }
}

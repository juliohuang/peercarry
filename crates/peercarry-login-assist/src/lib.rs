//! Login-assist policy and a separate native-host TLS adapter.
//!
//! Callers must authenticate devices and executors before invoking this engine.
//! Target comes from the trusted browser adapter, never from page/AI claims.
//! Time is a monotonic host clock in milliseconds, not client-provided time.
//! Credential values never enter Coordinator. The host adapter handles transient
//! inputs and crops, without importing any clipboard, history or storage API.
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Phase {
    Pausing,
    AwaitingUser,
    Controlling,
    Suspended,
    Revoking,
    Resuming,
    Completed,
    ResumeFailed,
    Cancelled,
    Expired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub device: String,
    pub browser: String,
    pub tab: u64,
    pub document: String,
    pub frame: u64,
    /// Exact origin, including scheme and non-default port, from browser APIs.
    pub origin: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
impl Rect {
    pub fn contains(self, x: u32, y: u32) -> bool {
        self.width > 0
            && self.height > 0
            && x >= self.x
            && y >= self.y
            && self
                .x
                .checked_add(self.width)
                .is_some_and(|right| x < right)
            && self
                .y
                .checked_add(self.height)
                .is_some_and(|bottom| y < bottom)
    }
    fn valid(self) -> bool {
        self.width > 0
            && self.height > 0
            && self.x.checked_add(self.width).is_some()
            && self.y.checked_add(self.height).is_some()
    }
}

#[derive(Clone, Debug)]
pub struct Scope {
    pub view: Rect,
    pub clicks: Option<Rect>,
    pub fields: HashSet<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Status {
    pub phase: Phase,
    pub login_completed: bool,
    pub ai_resumed: bool,
    pub epoch: u64,
    pub executor_required: bool,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Error {
    #[error("resource_busy")]
    Busy,
    #[error("not_found")]
    NotFound,
    #[error("invalid_state")]
    State,
    #[error("unauthorized")]
    Unauthorized,
    #[error("stale_binding")]
    Stale,
    #[error("expired")]
    Expired,
    #[error("duplicate_command")]
    Replay,
    #[error("out_of_scope")]
    Scope,
    #[error("invalid_request")]
    Invalid,
}

struct Session {
    target: Target,
    executor: String,
    phone: String,
    deadline: u64,
    status: Status,
    scope: Option<Scope>,
    commands: HashSet<u64>,
}

/// Single host policy coordinator. The whole device is locked, conservatively,
/// even for tab-only operations. A cancelled/expired session keeps its lock:
/// only an explicit trusted operator reset after executor reconciliation releases it.
#[derive(Default)]
pub struct Coordinator {
    sessions: HashMap<u64, Session>,
    devices: HashMap<String, u64>,
    next: u64,
}

impl Coordinator {
    pub fn request(
        &mut self,
        target: Target,
        executor: String,
        phone: String,
        now: u64,
        ttl: u64,
    ) -> Result<u64, Error> {
        if target.device.is_empty()
            || target.browser.is_empty()
            || target.document.is_empty()
            || executor.is_empty()
            || phone.is_empty()
            || ttl == 0
            || ttl > 300_000
            || !(target.origin.starts_with("https://") || target.origin == "http://127.0.0.1:5188")
        {
            return Err(Error::Invalid);
        }
        if self.devices.contains_key(&target.device) {
            return Err(Error::Busy);
        }
        let deadline = now.checked_add(ttl).ok_or(Error::Invalid)?;
        self.next = self.next.checked_add(1).ok_or(Error::Invalid)?;
        let id = self.next;
        self.devices.insert(target.device.clone(), id);
        self.sessions.insert(
            id,
            Session {
                target,
                executor,
                phone,
                deadline,
                status: Status {
                    phase: Phase::Pausing,
                    login_completed: false,
                    ai_resumed: false,
                    epoch: 1,
                    executor_required: true,
                },
                scope: None,
                commands: HashSet::new(),
            },
        );
        Ok(id)
    }

    fn live(&mut self, id: u64, now: u64) -> Result<&mut Session, Error> {
        let s = self.sessions.get_mut(&id).ok_or(Error::NotFound)?;
        if now >= s.deadline
            && !matches!(
                s.status.phase,
                Phase::Completed | Phase::Cancelled | Phase::Expired | Phase::ResumeFailed
            )
        {
            s.status.phase = Phase::Expired;
            s.scope = None;
            s.status.epoch += 1;
        }
        if s.status.phase == Phase::Expired {
            return Err(Error::Expired);
        }
        Ok(s)
    }

    /// Read-only AI result contains no origin, field values, images or cookies.
    pub fn status(&mut self, id: u64, executor: &str, now: u64) -> Result<Status, Error> {
        let s = self.sessions.get(&id).ok_or(Error::NotFound)?;
        if s.executor != executor {
            return Err(Error::Unauthorized);
        }
        let _ = self.live(id, now);
        Ok(self.sessions[&id].status)
    }

    /// Adapter must actually stop reads/screenshots/inputs before acknowledging.
    pub fn pause_ack(
        &mut self,
        id: u64,
        executor: &str,
        epoch: u64,
        now: u64,
    ) -> Result<(), Error> {
        let s = self.live(id, now)?;
        if s.executor != executor {
            return Err(Error::Unauthorized);
        }
        if s.status.epoch != epoch {
            return Err(Error::Stale);
        }
        if s.status.phase != Phase::Pausing {
            return Err(Error::State);
        }
        s.status.phase = Phase::AwaitingUser;
        Ok(())
    }

    pub fn approve(
        &mut self,
        id: u64,
        phone: &str,
        target: &Target,
        scope: Scope,
        now: u64,
    ) -> Result<u64, Error> {
        let s = self.live(id, now)?;
        if s.phone != phone {
            return Err(Error::Unauthorized);
        }
        if &s.target != target {
            return Err(Error::Stale);
        }
        if s.status.phase != Phase::AwaitingUser {
            return Err(Error::State);
        }
        if !scope.view.valid() || scope.clicks.is_some_and(|r| !r.valid()) {
            return Err(Error::Scope);
        }
        s.scope = Some(scope);
        s.status.phase = Phase::Controlling;
        Ok(s.status.epoch)
    }

    /// Changing view scope does not grant any additional input/click permission.
    /// Every update invalidates old commands and captured-frame coordinates.
    pub fn approve_view(
        &mut self,
        id: u64,
        phone: &str,
        epoch: u64,
        view: Rect,
        now: u64,
    ) -> Result<u64, Error> {
        let s = self.live(id, now)?;
        if s.phone != phone {
            return Err(Error::Unauthorized);
        }
        if s.status.epoch != epoch {
            return Err(Error::Stale);
        }
        if s.status.phase != Phase::Controlling {
            return Err(Error::State);
        }
        if !view.valid() {
            return Err(Error::Scope);
        }
        s.scope.as_mut().ok_or(Error::State)?.view = view;
        s.status.epoch += 1;
        Ok(s.status.epoch)
    }

    /// Reserve each operation before execution. Even execution failure must not
    /// retry the same command (especially OTP/password/submit operations).
    pub fn authorize(
        &mut self,
        id: u64,
        phone: &str,
        target: &Target,
        epoch: u64,
        command: u64,
        operation: Operation<'_>,
        now: u64,
    ) -> Result<(), Error> {
        let s = self.live(id, now)?;
        if s.phone != phone {
            return Err(Error::Unauthorized);
        }
        if &s.target != target || s.status.epoch != epoch {
            return Err(Error::Stale);
        }
        if s.status.phase != Phase::Controlling {
            return Err(Error::State);
        }
        if s.commands.contains(&command) {
            return Err(Error::Replay);
        }
        if s.commands.len() >= 1024 {
            return Err(Error::Scope);
        }
        let scope = s.scope.as_ref().ok_or(Error::State)?;
        match operation {
            Operation::Fill(field) if scope.fields.contains(field) => {}
            Operation::Click { x, y }
                if scope.view.contains(x, y) && scope.clicks.is_some_and(|r| r.contains(x, y)) => {}
            _ => return Err(Error::Scope),
        }
        s.commands.insert(command);
        Ok(())
    }

    /// Host-side crop of an in-memory RGBA fixture/capture. Native capture must
    /// refresh the epoch on geometry changes before supplying another frame.
    /// Caller owns transient buffers; this method never logs or persists pixels.
    pub fn crop_rgba(
        &mut self,
        id: u64,
        phone: &str,
        target: &Target,
        epoch: u64,
        width: u32,
        height: u32,
        pixels: &[u8],
        now: u64,
    ) -> Result<Vec<u8>, Error> {
        let s = self.live(id, now)?;
        if s.phone != phone {
            return Err(Error::Unauthorized);
        }
        if &s.target != target || s.status.epoch != epoch {
            return Err(Error::Stale);
        }
        if s.status.phase != Phase::Controlling {
            return Err(Error::State);
        }
        let view = s.scope.as_ref().ok_or(Error::State)?.view;
        let length = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|n| n.checked_mul(4))
            .ok_or(Error::Scope)?;
        if length > 64 * 1024 * 1024
            || length != pixels.len() as u64
            || view.x.checked_add(view.width).is_none_or(|v| v > width)
            || view.y.checked_add(view.height).is_none_or(|v| v > height)
        {
            return Err(Error::Scope);
        }
        let mut cropped = Vec::with_capacity(view.width as usize * view.height as usize * 4);
        for row in view.y..view.y + view.height {
            let start = (row as usize * width as usize + view.x as usize) * 4;
            cropped.extend_from_slice(&pixels[start..start + view.width as usize * 4]);
        }
        Ok(cropped)
    }

    /// Adapter calls on navigation, document/frame replacement, window/zoom
    /// change, or loss of the authenticated phone connection. No auto-resume.
    pub fn invalidate(&mut self, id: u64) -> Result<(), Error> {
        let s = self.sessions.get_mut(&id).ok_or(Error::NotFound)?;
        if matches!(
            s.status.phase,
            Phase::Completed | Phase::Cancelled | Phase::Expired | Phase::ResumeFailed
        ) {
            return Err(Error::State);
        }
        s.status.phase = Phase::Suspended;
        s.scope = None;
        s.status.epoch += 1;
        Ok(())
    }

    pub fn cancel(&mut self, id: u64, executor: &str) -> Result<(), Error> {
        let s = self.sessions.get_mut(&id).ok_or(Error::NotFound)?;
        if s.executor != executor {
            return Err(Error::Unauthorized);
        }
        if s.status.phase == Phase::Completed {
            return Err(Error::State);
        }
        s.scope = None;
        s.status.epoch += 1;
        s.status.phase = Phase::Cancelled;
        Ok(())
    }

    /// User's confirmation is not proof that the web session authenticated.
    /// `login_completed` means user-confirmed in P0, never inferred from idle CPU.
    pub fn hand_back(&mut self, id: u64, phone: &str, epoch: u64, now: u64) -> Result<u64, Error> {
        let s = self.live(id, now)?;
        if s.phone != phone {
            return Err(Error::Unauthorized);
        }
        if s.status.epoch != epoch {
            return Err(Error::Stale);
        }
        if s.status.phase != Phase::Controlling
            && (s.status.phase != Phase::Suspended || s.status.executor_required)
        {
            return Err(Error::State);
        }
        s.scope = None;
        s.status.epoch += 1;
        s.status.login_completed = true;
        s.status.phase = Phase::Revoking;
        Ok(s.status.epoch)
    }

    /// Trusted adapter confirms private stream closed, queued inputs/frames
    /// discarded and grants revoked. Only then may it invoke executor resume.
    pub fn revoked_ack(&mut self, id: u64, epoch: u64, now: u64) -> Result<(), Error> {
        let s = self.live(id, now)?;
        if s.status.epoch != epoch {
            return Err(Error::Stale);
        }
        if s.status.phase != Phase::Revoking {
            return Err(Error::State);
        }
        s.status.phase = Phase::Resuming;
        Ok(())
    }

    pub fn resume_ack(
        &mut self,
        id: u64,
        executor: &str,
        epoch: u64,
        success: bool,
        now: u64,
    ) -> Result<(), Error> {
        let s = self.live(id, now)?;
        if s.executor != executor {
            return Err(Error::Unauthorized);
        }
        if s.status.epoch != epoch {
            return Err(Error::Stale);
        }
        if s.status.phase != Phase::Resuming || !s.status.executor_required {
            return Err(Error::State);
        }
        s.status.ai_resumed = success;
        s.status.phase = if success {
            Phase::Completed
        } else {
            Phase::ResumeFailed
        };
        let device = s.target.device.clone();
        if success {
            self.devices.remove(&device);
        }
        Ok(())
    }

    /// A local human explicitly starts this mode; it does not claim an AI paused.
    pub fn request_manual(
        &mut self,
        target: Target,
        phone: String,
        now: u64,
        ttl: u64,
    ) -> Result<u64, Error> {
        let id = self.request(target, "manual".into(), phone, now, ttl)?;
        let s = self.sessions.get_mut(&id).ok_or(Error::NotFound)?;
        s.status.executor_required = false;
        s.status.phase = Phase::AwaitingUser;
        Ok(id)
    }

    pub fn manual_complete(&mut self, id: u64, epoch: u64, now: u64) -> Result<(), Error> {
        let s = self.live(id, now)?;
        if s.status.executor_required
            || s.status.epoch != epoch
            || s.status.phase != Phase::Resuming
        {
            return Err(Error::State);
        }
        s.status.phase = Phase::Completed;
        let device = s.target.device.clone();
        self.devices.remove(&device);
        Ok(())
    }

    /// Navigation needs fresh browser metadata and another user approval.
    pub fn rebind_manual(&mut self, id: u64, target: Target, now: u64) -> Result<(), Error> {
        let s = self.live(id, now)?;
        if s.status.executor_required
            || s.status.phase != Phase::Suspended
            || s.target.device != target.device
            || s.target.browser != target.browser
            || s.target.tab != target.tab
            || target.frame != 0
            || !(target.origin.starts_with("https://") || target.origin == "http://127.0.0.1:5188")
        {
            return Err(Error::State);
        }
        s.target = target;
        s.status.phase = Phase::AwaitingUser;
        Ok(())
    }
}

pub enum Operation<'a> {
    Fill(&'a str),
    Click { x: u32, y: u32 },
}

#[cfg(test)]
mod tests;

pub mod host;
pub mod native;

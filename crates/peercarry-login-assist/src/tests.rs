use super::*;

fn target() -> Target {
    Target {
        device: "test-mac".into(),
        browser: "test-chrome".into(),
        tab: 1,
        document: "document-1".into(),
        frame: 0,
        origin: "http://127.0.0.1:5188".into(),
    }
}
fn rect() -> Rect {
    Rect {
        x: 10,
        y: 10,
        width: 100,
        height: 100,
    }
}
fn scope() -> Scope {
    Scope {
        view: rect(),
        clicks: Some(rect()),
        fields: HashSet::from(["password".into()]),
    }
}
fn requested() -> (Coordinator, u64) {
    let mut c = Coordinator::default();
    let id = c
        .request(target(), "ai".into(), "phone".into(), 0, 1000)
        .unwrap();
    (c, id)
}
fn active() -> (Coordinator, u64) {
    let (mut c, id) = requested();
    c.pause_ack(id, "ai", 1, 1).unwrap();
    c.approve(id, "phone", &target(), scope(), 2).unwrap();
    (c, id)
}
fn fill(c: &mut Coordinator, id: u64, epoch: u64, cmd: u64, now: u64) -> Result<(), Error> {
    c.authorize(
        id,
        "phone",
        &target(),
        epoch,
        cmd,
        Operation::Fill("password"),
        now,
    )
}

#[test]
fn host_crop_contains_only_approved_pixels() {
    let (mut c, id) = requested();
    c.pause_ack(id, "ai", 1, 1).unwrap();
    let mut s = scope();
    s.view = Rect {
        x: 1,
        y: 0,
        width: 1,
        height: 2,
    };
    c.approve(id, "phone", &target(), s, 2).unwrap();
    let pixels: Vec<u8> = (0..24).collect();
    let crop = c
        .crop_rgba(id, "phone", &target(), 1, 3, 2, &pixels, 3)
        .unwrap();
    assert_eq!(crop, [4, 5, 6, 7, 16, 17, 18, 19]);
    c.invalidate(id).unwrap();
    assert!(c
        .crop_rgba(id, "phone", &target(), 1, 3, 2, &pixels, 4)
        .is_err());
}
#[test]
fn crop_rejects_short_frame_and_wrong_phone() {
    let (mut c, id) = active();
    assert_eq!(
        c.crop_rgba(id, "phone", &target(), 1, 120, 120, &[0; 4], 3),
        Err(Error::Scope)
    );
    assert_eq!(
        c.crop_rgba(id, "other", &target(), 1, 120, 120, &[0; 4], 3),
        Err(Error::Unauthorized)
    );
}

#[test]
fn approval_waits_for_actual_pause_ack() {
    let (mut c, id) = requested();
    assert_eq!(
        c.approve(id, "phone", &target(), scope(), 1),
        Err(Error::State)
    );
    assert_eq!(fill(&mut c, id, 1, 1, 1), Err(Error::State));
}
#[test]
fn other_executor_cannot_ack_pause() {
    let (mut c, id) = requested();
    assert_eq!(c.pause_ack(id, "other", 1, 1), Err(Error::Unauthorized));
}
#[test]
fn other_phone_cannot_approve() {
    let (mut c, id) = requested();
    c.pause_ack(id, "ai", 1, 1).unwrap();
    assert_eq!(
        c.approve(id, "other", &target(), scope(), 2),
        Err(Error::Unauthorized)
    );
}
#[test]
fn whole_device_is_mutually_exclusive() {
    let (mut c, _) = active();
    let mut t = target();
    t.tab = 2;
    assert_eq!(
        c.request(t, "other-ai".into(), "phone".into(), 3, 1000),
        Err(Error::Busy)
    );
}
#[test]
fn other_device_is_independent() {
    let (mut c, _) = active();
    let mut t = target();
    t.device = "other".into();
    assert!(c.request(t, "ai".into(), "phone".into(), 3, 1000).is_ok());
}
#[test]
fn changed_document_rejects_old_input() {
    let (mut c, id) = active();
    let mut t = target();
    t.document = "new".into();
    assert_eq!(
        c.authorize(id, "phone", &t, 1, 1, Operation::Fill("password"), 3),
        Err(Error::Stale)
    );
}
#[test]
fn changed_origin_rejects_input() {
    let (mut c, id) = active();
    let mut t = target();
    t.origin = "https://other.example".into();
    assert_eq!(
        c.authorize(id, "phone", &t, 1, 1, Operation::Fill("password"), 3),
        Err(Error::Stale)
    );
}
#[test]
fn non_authorized_fields_are_denied() {
    let (mut c, id) = active();
    assert_eq!(
        c.authorize(id, "phone", &target(), 1, 1, Operation::Fill("other"), 3),
        Err(Error::Scope)
    );
}
#[test]
fn click_bounds_are_half_open_and_overflow_safe() {
    let (mut c, id) = active();
    assert!(c
        .authorize(
            id,
            "phone",
            &target(),
            1,
            1,
            Operation::Click { x: 10, y: 10 },
            3
        )
        .is_ok());
    assert_eq!(
        c.authorize(
            id,
            "phone",
            &target(),
            1,
            2,
            Operation::Click { x: 110, y: 10 },
            3
        ),
        Err(Error::Scope)
    );
    assert!(!Rect {
        x: u32::MAX,
        y: 0,
        width: 2,
        height: 2
    }
    .contains(u32::MAX, 0));
}
#[test]
fn view_expansion_does_not_expand_clicks() {
    let (mut c, id) = active();
    let epoch = c
        .approve_view(
            id,
            "phone",
            1,
            Rect {
                x: 0,
                y: 0,
                width: 500,
                height: 500,
            },
            3,
        )
        .unwrap();
    assert_eq!(
        c.authorize(
            id,
            "phone",
            &target(),
            epoch,
            1,
            Operation::Click { x: 200, y: 200 },
            4
        ),
        Err(Error::Scope)
    );
    assert_eq!(fill(&mut c, id, 1, 2, 4), Err(Error::Stale));
}
#[test]
fn command_not_replayed_even_after_scope_change() {
    let (mut c, id) = active();
    fill(&mut c, id, 1, 5, 3).unwrap();
    let e = c.approve_view(id, "phone", 1, rect(), 4).unwrap();
    assert_eq!(fill(&mut c, id, e, 5, 5), Err(Error::Replay));
}
#[test]
fn disconnected_session_cannot_resume_or_fill() {
    let (mut c, id) = active();
    c.invalidate(id).unwrap();
    assert_eq!(fill(&mut c, id, 2, 1, 3), Err(Error::State));
    assert_eq!(c.resume_ack(id, "ai", 2, true, 3), Err(Error::State));
    assert!(!c.status(id, "ai", 3).unwrap().ai_resumed);
}
#[test]
fn expiry_revokes_and_holds_lock() {
    let (mut c, id) = active();
    assert_eq!(fill(&mut c, id, 1, 1, 1000), Err(Error::Expired));
    assert_eq!(c.status(id, "ai", 1001).unwrap().phase, Phase::Expired);
    assert_eq!(
        c.request(target(), "ai".into(), "phone".into(), 1001, 1000),
        Err(Error::Busy)
    );
}
#[test]
fn cancellation_does_not_restore_ai_or_unlock_device() {
    let (mut c, id) = active();
    c.cancel(id, "ai").unwrap();
    assert!(!c.status(id, "ai", 3).unwrap().ai_resumed);
    assert_eq!(fill(&mut c, id, 2, 1, 3), Err(Error::State));
    assert_eq!(
        c.request(target(), "ai".into(), "phone".into(), 4, 1000),
        Err(Error::Busy)
    );
}
#[test]
fn handback_requires_private_channel_revocation_ack() {
    let (mut c, id) = active();
    let e = c.hand_back(id, "phone", 1, 3).unwrap();
    assert_eq!(fill(&mut c, id, e, 1, 4), Err(Error::State));
    assert_eq!(c.resume_ack(id, "ai", e, true, 4), Err(Error::State));
    c.revoked_ack(id, e, 5).unwrap();
    c.resume_ack(id, "ai", e, true, 6).unwrap();
    assert!(c.status(id, "ai", 7).unwrap().ai_resumed);
    assert!(c
        .request(target(), "ai".into(), "phone".into(), 8, 1000)
        .is_ok());
}
#[test]
fn login_success_and_resume_failure_are_distinct() {
    let (mut c, id) = active();
    let e = c.hand_back(id, "phone", 1, 3).unwrap();
    c.revoked_ack(id, e, 4).unwrap();
    c.resume_ack(id, "ai", e, false, 5).unwrap();
    let s = c.status(id, "ai", 6).unwrap();
    assert!(s.login_completed);
    assert!(!s.ai_resumed);
    assert_eq!(s.phase, Phase::ResumeFailed);
}
#[test]
fn ai_cannot_read_other_executors_status() {
    let (mut c, id) = active();
    assert_eq!(c.status(id, "other", 3), Err(Error::Unauthorized));
}
#[test]
fn expired_or_forged_resume_ack_never_completes() {
    let (mut c, id) = active();
    let e = c.hand_back(id, "phone", 1, 3).unwrap();
    c.revoked_ack(id, e, 4).unwrap();
    assert_eq!(
        c.resume_ack(id, "other", e, true, 5),
        Err(Error::Unauthorized)
    );
    assert_eq!(c.resume_ack(id, "ai", e + 1, true, 5), Err(Error::Stale));
    assert_eq!(c.resume_ack(id, "ai", e, true, 1000), Err(Error::Expired));
}

#[test]
fn resume_failure_remains_terminal_after_deadline_and_invalidation() {
    let (mut c, id) = active();
    let epoch = c.hand_back(id, "phone", 1, 3).unwrap();
    c.revoked_ack(id, epoch, 4).unwrap();
    c.resume_ack(id, "ai", epoch, false, 5).unwrap();
    assert_eq!(c.status(id, "ai", 1001).unwrap().phase, Phase::ResumeFailed);
    assert_eq!(c.invalidate(id), Err(Error::State));
    assert_eq!(c.status(id, "ai", 1002).unwrap().epoch, epoch);
}

#[test]
fn manual_handoff_never_claims_ai_resumed() {
    let mut c = Coordinator::default();
    let id = c.request_manual(target(), "phone".into(), 0, 1000).unwrap();
    c.approve(id, "phone", &target(), scope(), 1).unwrap();
    let epoch = c.hand_back(id, "phone", 1, 2).unwrap();
    assert_eq!(c.manual_complete(id, epoch, 3), Err(Error::State));
    c.revoked_ack(id, epoch, 3).unwrap();
    assert_eq!(
        c.resume_ack(id, "manual", epoch, true, 4),
        Err(Error::State)
    );
    c.manual_complete(id, epoch, 4).unwrap();
    let status = c.status(id, "manual", 5).unwrap();
    assert_eq!(status.phase, Phase::Completed);
    assert!(!status.ai_resumed && !status.executor_required);
}

#[test]
fn manual_login_navigation_can_hand_back_without_reauthorizing_input() {
    let mut c = Coordinator::default();
    let id = c.request_manual(target(), "phone".into(), 0, 1000).unwrap();
    c.approve(id, "phone", &target(), scope(), 1).unwrap();
    c.invalidate(id).unwrap();
    assert!(fill(&mut c, id, 2, 1, 2).is_err());
    let epoch = c.hand_back(id, "phone", 2, 3).unwrap();
    c.revoked_ack(id, epoch, 4).unwrap();
    c.manual_complete(id, epoch, 5).unwrap();
    assert_eq!(c.status(id, "manual", 6).unwrap().phase, Phase::Completed);
}

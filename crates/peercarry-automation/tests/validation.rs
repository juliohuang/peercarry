use peercarry_automation::{
    parse_and_validate, Action, Assertion, AutomationError, Backend, Plan, Runner, Selector,
    WindowTarget,
};

fn example() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../examples/automation/focus-prompt.json"
    ))
    .unwrap()
}

#[test]
fn rejects_missing_variable_before_backend_access() {
    let mut value = example();
    value["steps"][0] = serde_json::json!({"id":"input", "action":"input_text", "value":{"kind":"variable","name":"absent"}});
    assert!(parse_and_validate(&value.to_string()).is_err());
}

#[test]
fn rejects_invalid_selector_region_independently() {
    let mut value = example();
    value["steps"][0]["selector"]["region"] =
        serde_json::json!({"left":5,"right":4,"top":0,"bottom":1});
    assert!(parse_and_validate(&value.to_string()).is_err());
}

#[test]
fn rejects_duplicate_step_ids() {
    let mut value = example();
    value["steps"][1]["id"] = value["steps"][0]["id"].clone();
    assert!(parse_and_validate(&value.to_string()).is_err());
}

#[test]
fn rejects_unknown_role_and_zero_timeout() {
    let mut value = example();
    value["steps"][0]["selector"]["role"] = "guess".into();
    assert!(parse_and_validate(&value.to_string()).is_err());
    let mut value = example();
    value["steps"][0]["timeout_ms"] = 0.into();
    assert!(parse_and_validate(&value.to_string()).is_err());
}

struct NoAccess;
impl Backend for NoAccess {
    fn resolve_target(&mut self, _: &WindowTarget) -> Result<(), AutomationError> {
        panic!("backend accessed")
    }
    fn focus(&mut self, _: &Selector) -> Result<(), AutomationError> {
        panic!("backend accessed")
    }
    fn input_text(&mut self, _: &str) -> Result<(), AutomationError> {
        panic!("backend accessed")
    }
    fn paste(&mut self, _: &str) -> Result<(), AutomationError> {
        panic!("backend accessed")
    }
    fn click(&mut self, _: &Selector) -> Result<(), AutomationError> {
        panic!("backend accessed")
    }
    fn expand(&mut self, _: &Selector) -> Result<(), AutomationError> {
        panic!("backend accessed")
    }
    fn assert_(&mut self, _: &Assertion) -> Result<(), AutomationError> {
        panic!("backend accessed")
    }
}

#[test]
fn stop_and_validation_precede_any_target_access() {
    let mut plan: Plan = parse_and_validate(&example().to_string()).unwrap();
    let mut runner = Runner::new(NoAccess, false);
    runner.stop_token().stop();
    assert!(matches!(
        runner.run(&plan, true),
        Err(AutomationError::Stopped)
    ));
    plan.steps[0].action = Action::Submit {
        selector: Selector {
            role: "button".into(),
            name: Some("Send".into()),
            automation_id: None,
            region: None,
        },
    };
    assert!(matches!(
        runner.run(&plan, true),
        Err(AutomationError::InvalidPlan(_))
    ));
}

#[test]
fn validation_only_never_accesses_desktop() {
    let plan = parse_and_validate(&example().to_string()).unwrap();
    for dry_run in [false, true] {
        Runner::new(NoAccess, dry_run).run(&plan, false).unwrap();
    }
}

#[test]
fn elapsed_timeout_prevents_next_step_but_does_not_undo_action() {
    use std::{cell::Cell, rc::Rc, time::Duration};
    struct SlowFocus(Rc<Cell<usize>>);
    impl Backend for SlowFocus {
        fn resolve_target(&mut self, _: &WindowTarget) -> Result<(), AutomationError> {
            Ok(())
        }
        fn focus(&mut self, _: &Selector) -> Result<(), AutomationError> {
            self.0.set(self.0.get() + 1);
            std::thread::sleep(Duration::from_millis(20));
            Ok(())
        }
        fn input_text(&mut self, _: &str) -> Result<(), AutomationError> {
            panic!("unexpected input")
        }
        fn paste(&mut self, _: &str) -> Result<(), AutomationError> {
            panic!("unexpected paste")
        }
        fn click(&mut self, _: &Selector) -> Result<(), AutomationError> {
            panic!("unexpected click")
        }
        fn expand(&mut self, _: &Selector) -> Result<(), AutomationError> {
            panic!("unexpected expand")
        }
        fn assert_(&mut self, _: &Assertion) -> Result<(), AutomationError> {
            panic!("continued after timeout")
        }
    }
    let mut plan = parse_and_validate(&example().to_string()).unwrap();
    plan.steps[0].timeout_ms = 1;
    let performed = Rc::new(Cell::new(0));
    let error = Runner::new(SlowFocus(performed.clone()), false)
        .run(&plan, true)
        .unwrap_err();
    assert!(matches!(error, AutomationError::Timeout(1)));
    assert_eq!(
        performed.get(),
        1,
        "timeout cannot undo a completed native call"
    );
}

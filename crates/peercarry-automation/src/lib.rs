//! A small, declarative local desktop-flow runner.
//! The default runner is a dry-run; platform adapters must explicitly opt into input.

use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fmt,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use thiserror::Error;

#[cfg(target_os = "windows")]
pub mod windows;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub version: u32,
    pub name: String,
    pub target: WindowTarget,
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowTarget {
    /// Exact executable name, without a path or wildcard.
    pub exe: String,
    /// Exact window title. Empty titles are not accepted because they are ambiguous.
    pub title: String,
    #[serde(default)]
    pub region: Option<Rect>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selector {
    pub role: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub automation_id: Option<String>,
    #[serde(default)]
    pub region: Option<Rect>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    Focus {
        selector: Selector,
    },
    InputText {
        value: ValueRef,
    },
    Paste {
        value: ValueRef,
    },
    Click {
        selector: Selector,
    },
    Expand {
        selector: Selector,
    },
    Assert {
        assertion: Assertion,
    },
    /// Kept explicit so unsafe plans fail validation with a useful error.
    Submit {
        selector: Selector,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    #[serde(flatten)]
    pub action: Action,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}
fn default_timeout_ms() -> u64 {
    5_000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ValueRef {
    Literal { value: String },
    Variable { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Assertion {
    ElementPresent {
        selector: Selector,
    },
    ElementName {
        selector: Selector,
        expected: String,
    },
    TextEquals {
        selector: Selector,
        expected: String,
    },
}

#[derive(Debug, Error)]
pub enum AutomationError {
    #[error("invalid plan: {0}")]
    InvalidPlan(String),
    #[error("target window {0}")]
    Target(String),
    #[error("step {step}: {message}")]
    Step { step: String, message: String },
    #[error("unsupported on this platform: {0}")]
    Unsupported(String),
    #[error("execution stopped")]
    Stopped,
    #[error("step timed out after {0} ms")]
    Timeout(u64),
}

pub fn parse_and_validate(json: &str) -> Result<Plan, AutomationError> {
    let plan: Plan =
        serde_json::from_str(json).map_err(|e| AutomationError::InvalidPlan(e.to_string()))?;
    plan.validate()?;
    Ok(plan)
}

impl Plan {
    pub fn validate(&self) -> Result<(), AutomationError> {
        if self.version != 1 {
            return Err(AutomationError::InvalidPlan("version must be 1".into()));
        }
        if self.name.trim().is_empty() {
            return Err(AutomationError::InvalidPlan("name is required".into()));
        }
        if self.target.exe.trim().is_empty() || self.target.exe.contains(['*', '?', '/', '\\']) {
            return Err(AutomationError::InvalidPlan(
                "exe must be a bare exact executable name".into(),
            ));
        }
        if self.target.title.trim().is_empty() {
            return Err(AutomationError::InvalidPlan(
                "window title must be exact and non-empty".into(),
            ));
        }
        if self.steps.is_empty() {
            return Err(AutomationError::InvalidPlan("steps cannot be empty".into()));
        }
        if let Some(r) = self.target.region {
            validate_rect(r)?;
        }
        let mut ids = std::collections::HashSet::new();
        for step in &self.steps {
            if step.id.trim().is_empty() || step.timeout_ms == 0 {
                return Err(AutomationError::InvalidPlan(format!(
                    "step {} has invalid id or timeout",
                    step.id
                )));
            }
            if !ids.insert(&step.id) {
                return Err(AutomationError::InvalidPlan(format!(
                    "duplicate step id {}",
                    step.id
                )));
            }
            validate_action(&step.action)?;
            validate_values(&step.action, &self.variables)?;
        }
        Ok(())
    }
}
fn validate_rect(r: Rect) -> Result<(), AutomationError> {
    if r.right <= r.left || r.bottom <= r.top {
        Err(AutomationError::InvalidPlan(
            "rectangle must have positive size".into(),
        ))
    } else {
        Ok(())
    }
}
fn validate_values(a: &Action, vars: &BTreeMap<String, String>) -> Result<(), AutomationError> {
    if let Action::InputText { value } | Action::Paste { value } = a {
        if let ValueRef::Variable { name } = value {
            if !vars.contains_key(name) {
                return Err(AutomationError::InvalidPlan(format!(
                    "missing variable {name}"
                )));
            }
        }
    }
    Ok(())
}
fn validate_selector(s: &Selector) -> Result<(), AutomationError> {
    if s.role.trim().is_empty() {
        return Err(AutomationError::InvalidPlan(
            "selector role is required".into(),
        ));
    }
    if !matches!(
        s.role.as_str(),
        "window"
            | "edit"
            | "text"
            | "button"
            | "combo_box"
            | "list"
            | "list_item"
            | "menu"
            | "menu_item"
    ) {
        return Err(AutomationError::InvalidPlan(format!(
            "unsupported selector role {}",
            s.role
        )));
    }
    if let Some(r) = s.region {
        validate_rect(r)?;
    }
    if s.name.as_deref().unwrap_or("").trim().is_empty()
        && s.automation_id.as_deref().unwrap_or("").trim().is_empty()
    {
        return Err(AutomationError::InvalidPlan(
            "selector requires name or automation_id".into(),
        ));
    }
    Ok(())
}
fn validate_action(a: &Action) -> Result<(), AutomationError> {
    match a {
        Action::Focus { selector }
        | Action::Click { selector }
        | Action::Expand { selector }
        | Action::Submit { selector } => validate_selector(selector)?,
        Action::Assert { assertion } => match assertion {
            Assertion::ElementPresent { selector }
            | Assertion::ElementName { selector, .. }
            | Assertion::TextEquals { selector, .. } => validate_selector(selector)?,
        },
        Action::InputText { value } | Action::Paste { value } => {
            if let ValueRef::Variable { name } = value {
                if name.trim().is_empty() {
                    return Err(AutomationError::InvalidPlan(
                        "variable name is empty".into(),
                    ));
                }
            }
        }
    }
    if matches!(a, Action::Submit { .. }) {
        return Err(AutomationError::InvalidPlan(
            "submit actions are refused by the P1 prototype".into(),
        ));
    }
    Ok(())
}

pub trait Backend {
    fn resolve_target(&mut self, target: &WindowTarget) -> Result<(), AutomationError>;
    fn focus(&mut self, selector: &Selector) -> Result<(), AutomationError>;
    fn input_text(&mut self, text: &str) -> Result<(), AutomationError>;
    fn paste(&mut self, text: &str) -> Result<(), AutomationError>;
    fn click(&mut self, selector: &Selector) -> Result<(), AutomationError>;
    fn expand(&mut self, selector: &Selector) -> Result<(), AutomationError>;
    fn assert_(&mut self, assertion: &Assertion) -> Result<(), AutomationError>;
}

#[derive(Clone, Default)]
pub struct StopToken(Arc<AtomicBool>);
impl StopToken {
    pub fn stop(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    fn stopped(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

pub struct Runner<B> {
    backend: B,
    pub dry_run: bool,
    stop: StopToken,
}
impl<B: Backend> Runner<B> {
    pub fn new(backend: B, dry_run: bool) -> Self {
        Self {
            backend,
            dry_run,
            stop: StopToken::default(),
        }
    }
    pub fn stop_token(&self) -> StopToken {
        self.stop.clone()
    }
    pub fn run(&mut self, plan: &Plan, execute: bool) -> Result<(), AutomationError> {
        plan.validate()?;
        if execute && self.dry_run {
            return Err(AutomationError::InvalidPlan(
                "execute requires a non-dry-run runner".into(),
            ));
        }
        if self.stop.stopped() {
            return Err(AutomationError::Stopped);
        }
        if !execute || self.dry_run {
            return Ok(());
        }
        self.backend.resolve_target(&plan.target)?;
        for step in &plan.steps {
            if self.stop.stopped() {
                return Err(AutomationError::Stopped);
            }
            let started = Instant::now();
            let result = if self.dry_run {
                Ok(())
            } else {
                self.run_action(&step.action, plan)
            };
            if started.elapsed() > Duration::from_millis(step.timeout_ms) {
                return Err(AutomationError::Timeout(step.timeout_ms));
            }
            result.map_err(|e| match e {
                AutomationError::Step { .. } | AutomationError::Timeout(_) => e,
                other => AutomationError::Step {
                    step: step.id.clone(),
                    message: other.to_string(),
                },
            })?;
        }
        Ok(())
    }
    fn run_action(&mut self, action: &Action, plan: &Plan) -> Result<(), AutomationError> {
        match action {
            Action::Focus { selector } => self.backend.focus(selector),
            Action::InputText { value } => self.backend.input_text(&resolve(value, plan)?),
            Action::Paste { value } => self.backend.paste(&resolve(value, plan)?),
            Action::Click { selector } => self.backend.click(selector),
            Action::Expand { selector } => self.backend.expand(selector),
            Action::Assert { assertion } => self.backend.assert_(assertion),
            Action::Submit { .. } => Err(AutomationError::InvalidPlan(
                "submit actions are refused by the P1 prototype".into(),
            )),
        }
    }
}
fn resolve(value: &ValueRef, plan: &Plan) -> Result<String, AutomationError> {
    match value {
        ValueRef::Literal { value } => Ok(value.clone()),
        ValueRef::Variable { name } => plan
            .variables
            .get(name)
            .cloned()
            .ok_or_else(|| AutomationError::InvalidPlan(format!("missing variable {name}"))),
    }
}

#[derive(Default)]
pub struct UnsupportedBackend;
impl Backend for UnsupportedBackend {
    fn resolve_target(&mut self, _: &WindowTarget) -> Result<(), AutomationError> {
        Err(AutomationError::Unsupported(
            "desktop automation backend is unavailable".into(),
        ))
    }
    fn focus(&mut self, _: &Selector) -> Result<(), AutomationError> {
        Err(AutomationError::Unsupported("focus".into()))
    }
    fn input_text(&mut self, _: &str) -> Result<(), AutomationError> {
        Err(AutomationError::Unsupported("input".into()))
    }
    fn paste(&mut self, _: &str) -> Result<(), AutomationError> {
        Err(AutomationError::Unsupported("paste".into()))
    }
    fn click(&mut self, _: &Selector) -> Result<(), AutomationError> {
        Err(AutomationError::Unsupported("click".into()))
    }
    fn expand(&mut self, _: &Selector) -> Result<(), AutomationError> {
        Err(AutomationError::Unsupported("expand".into()))
    }
    fn assert_(&mut self, _: &Assertion) -> Result<(), AutomationError> {
        Err(AutomationError::Unsupported("assert".into()))
    }
}

impl fmt::Debug for StopToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StopToken").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn selector() -> Selector {
        Selector {
            role: "edit".into(),
            name: Some("Prompt".into()),
            automation_id: None,
            region: None,
        }
    }
    fn plan(action: Action) -> Plan {
        Plan {
            version: 1,
            name: "t".into(),
            target: WindowTarget {
                exe: "zcode.exe".into(),
                title: "Project".into(),
                region: None,
            },
            variables: BTreeMap::new(),
            steps: vec![Step {
                id: "one".into(),
                action,
                timeout_ms: 100,
            }],
        }
    }
    #[test]
    fn ambiguous_selector_rejected() {
        let mut p = plan(Action::Focus {
            selector: selector(),
        });
        if let Action::Focus { selector } = &mut p.steps[0].action {
            selector.name = None;
        }
        assert!(p.validate().is_err());
    }
    #[test]
    fn submit_rejected() {
        assert!(plan(Action::Submit {
            selector: selector()
        })
        .validate()
        .is_err());
    }
    #[test]
    fn dry_run_does_not_touch_backend() {
        let mut r = Runner::new(Recording::default(), true);
        assert!(r
            .run(
                &plan(Action::Click {
                    selector: selector()
                }),
                false
            )
            .is_ok());
        assert!(r.backend.calls.is_empty());
    }
    #[test]
    fn execute_flag_is_required_for_non_dry_runner() {
        let mut r = Runner::new(Recording::default(), false);
        assert!(r
            .run(
                &plan(Action::Click {
                    selector: selector()
                }),
                false
            )
            .is_ok());
        assert!(r.backend.calls.is_empty());
    }
    #[test]
    fn assertion_failure_stops_following_steps() {
        let mut p = plan(Action::Assert {
            assertion: Assertion::ElementPresent {
                selector: selector(),
            },
        });
        p.steps.push(Step {
            id: "later".into(),
            action: Action::Click {
                selector: selector(),
            },
            timeout_ms: 100,
        });
        let mut r = Runner::new(
            Recording {
                fail_assert: true,
                ..Default::default()
            },
            false,
        );
        assert!(r.run(&p, true).is_err());
        assert_eq!(r.backend.calls, vec!["target", "assert"]);
    }
    #[test]
    fn stop_prevents_execution() {
        let mut r = Runner::new(Recording::default(), false);
        r.stop_token().stop();
        assert!(matches!(
            r.run(
                &plan(Action::Click {
                    selector: selector()
                }),
                true
            ),
            Err(AutomationError::Stopped)
        ));
    }
    #[test]
    fn example_json_is_valid() {
        let p = parse_and_validate(include_str!(
            "../../../examples/automation/focus-prompt.json"
        ))
        .unwrap();
        assert_eq!(p.steps.len(), 2);
        parse_and_validate(include_str!(
            "../../../examples/automation/zcode-inspect.json"
        ))
        .unwrap();
        parse_and_validate(include_str!(
            "../../../examples/automation/zcode-empty-input.json"
        ))
        .unwrap();
    }
    #[test]
    fn duplicate_ids_missing_vars_and_bad_rect_rejected() {
        let mut p = plan(Action::Click {
            selector: selector(),
        });
        p.steps.push(p.steps[0].clone());
        assert!(p.validate().is_err());
        p.steps.truncate(1);
        p.steps[0].action = Action::Paste {
            value: ValueRef::Variable {
                name: "missing".into(),
            },
        };
        assert!(p.validate().is_err());
        p.target.region = Some(Rect {
            left: 1,
            top: 1,
            right: 1,
            bottom: 2,
        });
        assert!(p.validate().is_err());
    }
    #[derive(Default)]
    struct Recording {
        calls: Vec<&'static str>,
        fail_assert: bool,
    }
    impl Backend for Recording {
        fn resolve_target(&mut self, _: &WindowTarget) -> Result<(), AutomationError> {
            self.calls.push("target");
            Ok(())
        }
        fn focus(&mut self, _: &Selector) -> Result<(), AutomationError> {
            self.calls.push("focus");
            Ok(())
        }
        fn input_text(&mut self, _: &str) -> Result<(), AutomationError> {
            self.calls.push("input");
            Ok(())
        }
        fn paste(&mut self, _: &str) -> Result<(), AutomationError> {
            self.calls.push("paste");
            Ok(())
        }
        fn click(&mut self, _: &Selector) -> Result<(), AutomationError> {
            self.calls.push("click");
            Ok(())
        }
        fn expand(&mut self, _: &Selector) -> Result<(), AutomationError> {
            self.calls.push("expand");
            Ok(())
        }
        fn assert_(&mut self, _: &Assertion) -> Result<(), AutomationError> {
            self.calls.push("assert");
            if self.fail_assert {
                Err(AutomationError::Target("failed assertion".into()))
            } else {
                Ok(())
            }
        }
    }
}

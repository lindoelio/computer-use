//! Tier `activate` fallback for a UI Automation pattern that did not work.
//!
//! UIA control patterns are tier `ax`: they run in the background and never
//! move the system cursor. Some Win32 apps ignore `Invoke` until their window
//! is in front, though, so a pattern that is missing — or whose single call
//! fails — is retried here as `SetForegroundWindow` plus enigo input, under
//! the same `foreground.lock` every other foreground path uses.
//!
//! The decision itself is pure ([`plan`]) so the tier, the refusal reasons and
//! the warning selection are unit-tested without a desktop session.

#![allow(unsafe_code)]

use crate::element::{ElementOp, ElementOutcome, ElementRequest};
use crate::error::ComputerError;

/// Warning attached to an action the activate tier is what made work.
pub(crate) const FOREGROUND_REQUIRED: &str = "foreground required";

/// Why a UI Automation pattern could not complete the action.
///
/// Both variants are recoverable: some Win32 apps advertise `Invoke` but
/// ignore it until their window is foreground.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PatternFailure {
    /// The element does not advertise the pattern at all.
    Missing {
        /// UIA pattern name, for the message.
        pattern: &'static str,
        /// The `uiautomation` error text.
        detail: String,
    },
    /// The pattern exists but the single call failed.
    Call {
        /// UIA pattern name, for the message.
        pattern: &'static str,
        /// The `uiautomation` error text.
        detail: String,
    },
}

impl PatternFailure {
    /// One line naming the pattern and what went wrong.
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::Missing { pattern, detail } => {
                format!("uia:{pattern}: missing control pattern: {detail}")
            }
            Self::Call { pattern, detail } => format!("uia:{pattern}: call failed: {detail}"),
        }
    }
}

/// How the foreground lock resolved before the fallback runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LockRoute {
    /// `--allow-foreground` was passed, so `foreground.lock` is skipped.
    Allowed,
    /// This session holds `foreground.lock`.
    Acquired,
    /// Another session holds it; the pid of the holder.
    Busy(u32),
}

/// What the dispatcher should do after a pattern failed.
#[derive(Debug)]
pub(crate) enum FallbackPlan {
    /// Put the window in front and run enigo input; report tier `activate`.
    Activate,
    /// Do not fall back; surface this error instead.
    Refuse(ComputerError),
}

/// Decide whether a failed UIA pattern may be retried as tier `activate`.
///
/// Pure: no Win32 call, no lock, no input. The caller measures [`LockRoute`]
/// and passes the element centre it already has from the revalidation gate.
///
/// Rules, in order:
/// 1. `--require-background` refuses the fallback outright — putting the
///    window in front is exactly what that flag forbids.
/// 2. Another session holding `foreground.lock` refuses it, the same rule
///    P7 applies to a coordinate click.
/// 3. Only `press` and `type` can be reproduced faithfully through enigo.
///    `set_value` must go through `ValuePattern` (the P3 parity contract is
///    "value, not keystrokes"), `select_text` needs the `Text` range, and
///    `scroll_to` needs `ScrollItemPattern`; a synthesised gesture would be a
///    guess, not the action the caller asked for.
/// 4. `press` needs the element frame to aim the click.
pub(crate) fn plan(
    op: &ElementOp,
    failure: &PatternFailure,
    require_background: bool,
    route: LockRoute,
    center: Option<(i32, i32)>,
) -> FallbackPlan {
    if require_background {
        return FallbackPlan::Refuse(ComputerError::ExecutionFailed(format!(
            "--require-background refuses the activate fallback for {} because it puts the window \
             in front; {} was the only alternative",
            op.action_name(),
            failure.describe()
        )));
    }
    if let LockRoute::Busy(pid) = route {
        return FallbackPlan::Refuse(ComputerError::ExecutionFailed(format!(
            "another session (pid {pid}) holds foreground.lock, so the activate fallback for {} \
             was not attempted",
            op.action_name()
        )));
    }
    match op {
        ElementOp::Press { .. } => {
            if center.is_none() {
                return FallbackPlan::Refuse(ComputerError::ExecutionFailed(format!(
                    "the element has no frame, so the activate fallback cannot aim a click for {}",
                    op.action_name()
                )));
            }
            FallbackPlan::Activate
        }
        ElementOp::Type { .. } => FallbackPlan::Activate,
        other => FallbackPlan::Refuse(ComputerError::ExecutionFailed(format!(
            "the activate fallback only reproduces press and type; {} needs its UI Automation \
             pattern ({})",
            other.action_name(),
            failure.describe()
        ))),
    }
}

/// Warnings carried by an outcome the activate tier produced.
///
/// `foreground required` is emitted exactly when the activate retry is what
/// made the action work — which is the only case that reaches this function,
/// because the UIA pattern already failed and the fallback then succeeded.
pub(crate) fn activate_warnings(extra: &[String]) -> Vec<String> {
    let mut warnings = Vec::with_capacity(extra.len() + 1);
    warnings.push(FOREGROUND_REQUIRED.to_string());
    warnings.extend(extra.iter().cloned());
    warnings
}

/// The `ElementOutcome` for a successful activate fallback.
///
/// `focus_restored` reports what actually happened: whether the window that
/// was in front before the fallback is in front again.
pub(crate) fn outcome(
    request: &ElementRequest,
    revision: u64,
    focus_restored: bool,
    extra_warnings: &[String],
) -> ElementOutcome {
    let mut outcome = crate::element::activate_outcome(
        request,
        revision,
        activate_warnings(extra_warnings),
        focus_restored,
    );
    outcome.cursor_moved = true;
    outcome
}

/// Run the activate fallback for real: foreground the window, then enigo.
///
/// Returns whether the previously foreground window was put back. The test
/// build refuses, because a unit test must never move the system cursor or
/// bring a window to the front.
pub(crate) fn run(
    hwnd: u32,
    op: &ElementOp,
    gate: &crate::element::Gate,
) -> Result<bool, ComputerError> {
    #[cfg(test)]
    {
        let _ = (hwnd, op, gate);
        Err(ComputerError::ExecutionFailed(
            "foreground input is disabled in tests".into(),
        ))
    }
    #[cfg(not(test))]
    {
        let previous = foreground_window();
        set_foreground(hwnd)?;
        let action = match op {
            ElementOp::Press { button, count } => press(*button, *count, gate.center),
            ElementOp::Type { text } => type_into(hwnd, gate, text),
            // `plan` already refused everything else.
            other => {
                return Err(ComputerError::ExecutionFailed(format!(
                    "the activate fallback does not reproduce {}",
                    other.action_name()
                )))
            }
        };
        let restored = restore_foreground(previous);
        action.map(|()| restored)
    }
}

/// Click the element centre `count` times.
#[cfg(not(test))]
fn press(
    button: crate::types::MouseButton,
    count: u8,
    center: Option<(i32, i32)>,
) -> Result<(), ComputerError> {
    use std::time::Duration;

    let Some((x, y)) = center else {
        return Err(ComputerError::ExecutionFailed(
            "element has no frame, so the activate fallback cannot aim a click".into(),
        ));
    };
    for index in 0..count.max(1) {
        if index > 0 {
            std::thread::sleep(Duration::from_millis(50));
        }
        crate::backend::input::click(x, y, button)?;
    }
    Ok(())
}

/// Focus the element through UIA, then type into whatever holds focus.
///
/// `set_focus` is a background-safe UIA call, so the only foreground step is
/// the `SetForegroundWindow` that already happened.
#[cfg(not(test))]
fn type_into(hwnd: u32, gate: &crate::element::Gate, text: &str) -> Result<(), ComputerError> {
    super::uia::focus_element(hwnd, &gate.path)?;
    crate::backend::input::type_text(text)
}

/// The window currently in front, as an `HWND` id, or `None`.
#[cfg(not(test))]
pub(crate) fn foreground_window() -> Option<u32> {
    let handle = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
    (!handle.0.is_null()).then_some(handle.0 as u32)
}

/// Put `hwnd` in front, verifying the result rather than trusting the return.
///
/// Windows applies a foreground-lock timeout, so `SetForegroundWindow` can be
/// refused outright.
#[cfg(not(test))]
fn set_foreground(hwnd: u32) -> Result<(), ComputerError> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetForegroundWindow};
    let handle = HWND(hwnd as *mut std::ffi::c_void);
    let _accepted = unsafe { SetForegroundWindow(handle) };
    if unsafe { GetForegroundWindow() } == handle {
        return Ok(());
    }
    Err(ComputerError::ExecutionFailed(format!(
        "SetForegroundWindow did not bring hwnd {hwnd} to the front; Windows refused the \
         foreground change, so the activate fallback cannot continue"
    )))
}

/// Put the previous window back. `true` when it is in front again (or there
/// was nothing to put back).
#[cfg(not(test))]
fn restore_foreground(previous: Option<u32>) -> bool {
    match previous {
        Some(hwnd) => set_foreground(hwnd).is_ok(),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ComputerError;
    use crate::types::MouseButton;

    fn request(op: ElementOp, require_background: bool) -> ElementRequest {
        ElementRequest {
            op,
            index: 3,
            require_visible: false,
            app: Some("notepad".into()),
            window: None,
            session: None,
            tier: None,
            allow_foreground: false,
            require_background,
            no_overlay: false,
        }
    }

    fn press() -> ElementOp {
        ElementOp::Press {
            count: 1,
            button: MouseButton::Left,
        }
    }

    fn call_failure() -> PatternFailure {
        PatternFailure::Call {
            pattern: "Invoke",
            detail: "element not enabled".into(),
        }
    }

    fn missing(pattern: &'static str) -> PatternFailure {
        PatternFailure::Missing {
            pattern,
            detail: "the control does not expose the pattern".into(),
        }
    }

    #[test]
    fn a_failed_invoke_falls_back_to_activate() {
        let plan = plan(
            &press(),
            &call_failure(),
            false,
            LockRoute::Acquired,
            Some((10, 20)),
        );
        assert!(matches!(plan, FallbackPlan::Activate), "{plan:?}");
    }

    #[test]
    fn a_missing_pattern_falls_back_to_activate() {
        let plan = plan(
            &press(),
            &missing("Invoke"),
            false,
            LockRoute::Acquired,
            Some((10, 20)),
        );
        assert!(matches!(plan, FallbackPlan::Activate), "{plan:?}");
    }

    #[test]
    fn allow_foreground_still_falls_back() {
        let plan = plan(
            &press(),
            &call_failure(),
            false,
            LockRoute::Allowed,
            Some((10, 20)),
        );
        assert!(matches!(plan, FallbackPlan::Activate), "{plan:?}");
    }

    #[test]
    fn require_background_refuses_the_fallback() {
        let plan = plan(
            &press(),
            &call_failure(),
            true,
            LockRoute::Acquired,
            Some((10, 20)),
        );
        let FallbackPlan::Refuse(ComputerError::ExecutionFailed(message)) = plan else {
            panic!("expected a refusal, got {plan:?}");
        };
        assert!(message.contains("--require-background"), "{message}");
        assert!(message.contains("uia:Invoke"), "{message}");
    }

    #[test]
    fn a_busy_foreground_lock_refuses_the_fallback() {
        let plan = plan(
            &press(),
            &call_failure(),
            false,
            LockRoute::Busy(4242),
            Some((10, 20)),
        );
        let FallbackPlan::Refuse(ComputerError::ExecutionFailed(message)) = plan else {
            panic!("expected a refusal, got {plan:?}");
        };
        assert!(message.contains("4242"), "{message}");
        assert!(message.contains("foreground.lock"), "{message}");
    }

    #[test]
    fn press_without_a_frame_refuses_the_fallback() {
        let plan = plan(&press(), &call_failure(), false, LockRoute::Acquired, None);
        let FallbackPlan::Refuse(ComputerError::ExecutionFailed(message)) = plan else {
            panic!("expected a refusal, got {plan:?}");
        };
        assert!(message.contains("no frame"), "{message}");
    }

    #[test]
    fn value_ops_are_never_rewritten_as_keystrokes() {
        for op in [
            ElementOp::SetValue {
                value: "hello".into(),
            },
            ElementOp::SelectText { start: 0, end: 3 },
            ElementOp::ScrollTo,
            ElementOp::Focus,
        ] {
            let plan = plan(
                &op,
                &missing("Value"),
                false,
                LockRoute::Acquired,
                Some((10, 20)),
            );
            let FallbackPlan::Refuse(ComputerError::ExecutionFailed(message)) = plan else {
                panic!("expected a refusal for {op:?}");
            };
            assert!(message.contains("press and type"), "{message}");
        }
    }

    #[test]
    fn type_does_fall_back() {
        let op = ElementOp::Type {
            text: "hello".into(),
        };
        assert!(matches!(
            plan(
                &op,
                &missing("Value"),
                false,
                LockRoute::Acquired,
                Some((10, 20))
            ),
            FallbackPlan::Activate
        ));
        // `type --element` has no click to aim, so a missing frame is fine.
        assert!(matches!(
            plan(&op, &missing("Value"), false, LockRoute::Acquired, None),
            FallbackPlan::Activate
        ));
    }

    #[test]
    fn the_foreground_required_warning_is_attached_to_an_activate_outcome() {
        let warnings = activate_warnings(&["already known".into()]);
        assert_eq!(warnings[0], FOREGROUND_REQUIRED);
        assert_eq!(warnings.len(), 2);
        assert_eq!(warnings[1], "already known");
    }

    #[test]
    fn an_activate_outcome_reports_tier_activate_and_the_warning() {
        let result = outcome(&request(press(), false), 77, true, &[]);
        assert_eq!(result.tier, "activate");
        assert_eq!(result.via.as_deref(), Some("activate"));
        assert!(result.cursor_moved);
        assert_eq!(result.focus_restored, Some(true));
        assert_eq!(result.warnings, vec![FOREGROUND_REQUIRED.to_string()]);
        assert_eq!(result.element, 3);
        assert_eq!(result.revision, 77);
    }

    #[test]
    fn an_activate_outcome_can_report_a_failed_focus_restore() {
        let result = outcome(&request(press(), false), 5, false, &[]);
        assert_eq!(result.focus_restored, Some(false));
    }

    #[test]
    fn pattern_failures_describe_themselves() {
        assert_eq!(
            missing("ScrollItem").describe(),
            "uia:ScrollItem: missing control pattern: the control does not expose the pattern"
        );
        assert_eq!(
            call_failure().describe(),
            "uia:Invoke: call failed: element not enabled"
        );
    }
}

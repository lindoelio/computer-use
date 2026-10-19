//! Run the tier plan for one input job, including the event-to-activate fallback.

use crate::backend::DesktopBackend;
use crate::error::ComputerError;
use crate::foreground;
use crate::overlay;
use crate::tier::{self, InputJob, InputKind, InputReport, TierName};

/// Flags the tier loop needs. Callers copy them off a request or a context.
#[derive(Debug, Clone)]
pub struct AttemptFlags {
    /// Target application.
    pub app: Option<String>,
    /// Target window.
    pub window: Option<String>,
    /// Requested tier. Empty is auto.
    pub tier: Option<String>,
    /// Skip `foreground.lock` for `activate`.
    pub allow_foreground: bool,
    /// Do not fall back to `activate`, and fail if the cursor moves.
    pub require_background: bool,
    /// Skip the overlay helper.
    pub no_overlay: bool,
    /// Session id for the overlay point.
    pub session: Option<String>,
}

/// Plan and run `kind`. Event `execution_failed` falls back to `activate`
/// only when the lock is free and `--require-background` is unset.
pub fn run(
    backend: &dyn DesktopBackend,
    flags: &AttemptFlags,
    kind: InputKind,
) -> Result<InputReport, ComputerError> {
    let requested = tier::parse_tier(flags.tier.as_deref())?;
    let attempts = tier::plan(
        requested,
        tier::InputClass::Background,
        backend.input_support(),
        tier::skylight_compiled(),
    )?;
    let mut event_error: Option<ComputerError> = None;
    for (index, tier_name) in attempts.iter().copied().enumerate() {
        if tier_name == TierName::Activate {
            match activate_block(flags)? {
                Some(blocked) if index > 0 => {
                    return Err(event_error.unwrap_or(blocked));
                }
                Some(blocked) => return Err(blocked),
                None => {}
            }
        }
        let job = InputJob {
            tier: tier_name,
            kind: kind.clone(),
            app: flags.app.clone(),
            window: flags.window.clone(),
            allow_foreground: flags.allow_foreground,
            require_background: flags.require_background,
        };
        match backend.perform_input(&job) {
            Ok(mut report) => {
                if !flags.no_overlay {
                    if let Some((x, y)) = tier::input_point(&kind) {
                        let session = flags
                            .session
                            .clone()
                            .unwrap_or_else(|| std::process::id().to_string());
                        if let Some(warning) = overlay::note_point(x, y, &session) {
                            report.warnings.push(warning.to_string());
                        }
                    }
                }
                return Ok(report);
            }
            Err(error) => {
                let next_is_activate = attempts.get(index + 1) == Some(&TierName::Activate);
                if tier_name == TierName::Event
                    && error.code() == "execution_failed"
                    && next_is_activate
                {
                    let blocked = activate_block(flags)?;
                    let lock_held = blocked.as_ref().is_some_and(is_lock_error);
                    if tier::fallback_to_activate(lock_held, flags.require_background)
                        && blocked.is_none()
                    {
                        event_error = Some(error);
                        continue;
                    }
                }
                return Err(error);
            }
        }
    }
    Err(ComputerError::ExecutionFailed("no input tier ran".into()))
}

fn activate_block(flags: &AttemptFlags) -> Result<Option<ComputerError>, ComputerError> {
    if flags.require_background {
        return Ok(Some(ComputerError::ExecutionFailed(
            "--require-background refuses tier activate because it uses the foreground cursor"
                .into(),
        )));
    }
    if flags.allow_foreground {
        return Ok(None);
    }
    let Some(path) = foreground::lock_path() else {
        return Ok(None);
    };
    match foreground::holder_if_taken(&path) {
        Ok(None) => Ok(None),
        Ok(Some(pid)) if pid > 0 => Ok(Some(ComputerError::ExecutionFailed(format!(
            "foreground.lock is held by pid {pid}"
        )))),
        Ok(Some(_)) => Ok(Some(ComputerError::ExecutionFailed(
            "foreground.lock is held by another session".into(),
        ))),
        Err(error) => Err(error.into_computer()),
    }
}

fn is_lock_error(error: &ComputerError) -> bool {
    error.to_string().contains("foreground.lock")
}

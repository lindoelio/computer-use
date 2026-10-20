//! Scroll verification and the AX scroll-bar fallback. Pure: no events are
//! posted, no Accessibility call is made, and no clock is read from here.
//!
//! A scroll-wheel `CGEvent` posted with `CGEventPostToPid` is not delivered to
//! the views of a non-frontmost app, so a posted event is not proof of a
//! scroll. The event tier reads the `AXScrollArea` scroll bar before and after
//! the post, and this module turns that measurement into a decision: the wheel
//! event worked, the AX scroll-bar fallback should run, the bar is already at
//! its limit, or nothing could be verified at all.
//!
//! The per-line step size is computed from the ratio between the content
//! height (the document child of the `AXScrollArea`) and the visible height
//! (the scroll area itself, minus the orthogonal scroll bar). One wheel
//! notch is roughly three text lines, so a notch is [`LINE_POINTS`] points;
//! the math in [`content_step`] divides that by the content-overflow
//! (`content - visible`) and clamps to `0..=1` so a wheel line never jumps
//! the whole bar.
//!
//! Every function here is pure so the same tests run on macOS, Windows, and
//! Linux. The macOS backend owns the Accessibility calls
//! (`src/backend/macos/scroll_ax.rs`); Windows and Linux scroll through their
//! own foreground or portal paths and do not use this module.

/// Two scroll-bar values closer than this are the same value.
///
/// `AXValue` is a `double`, so a rounding wobble must not read as a scroll.
pub const EPSILON: f64 = 1e-6;

/// Warning text when the wheel event did nothing and `AXValue` was written.
pub const AX_FALLBACK_WARNING: &str =
    "wheel event not delivered to background app; used AX scroll bar";

/// Warning text when the scroll area has no measured `AXSize` for either the
/// scroll area itself or its document child, so the per-line step falls back
/// to the legacy five-percent approximation.
pub const APPROXIMATED_WARNING: &str = "scroll step approximated";

/// Warning text when the document's content fits inside the visible viewport,
/// so a scroll is not possible and the per-line step is `0.0`.
pub const CONTENT_FITS_WARNING: &str = "content fits; nothing to scroll";

/// Value step per wheel line when the content sizing data could not be read.
pub const DEFAULT_STEP: f64 = 0.05;

/// One wheel-line in points.
///
/// A typical macOS wheel notch is roughly three text lines, each about 13
/// points tall, so 40 points is the line budget per notch.
pub const LINE_POINTS: f64 = 40.0;

/// Smallest step per wheel line. Below this a write would not visibly move.
pub const MIN_STEP: f64 = 0.005;

/// Largest step per wheel line. One wheel line never jumps the whole bar.
pub const MAX_STEP: f64 = 1.0;

/// How far a read-back value may sit from the requested value before the
/// outcome notes that the app landed somewhere else. An app that snaps to its
/// own page is still a scroll; it just did not land where we asked.
pub const ROUNDING_TOLERANCE: f64 = 0.005;

/// One axis of a scroll bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Axis {
    /// `AXVerticalScrollBar`. Value 0 is the top.
    Vertical,
    /// `AXHorizontalScrollBar`. Value 0 is the left.
    Horizontal,
}

impl Axis {
    /// Stable JSON name used in warnings.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vertical => "vertical",
            Self::Horizontal => "horizontal",
        }
    }

    /// The edge `steps` is scrolling towards. Positive steps go down or right.
    pub fn limit(self, steps: i32) -> &'static str {
        match (self, steps >= 0) {
            (Self::Vertical, true) => "bottom",
            (Self::Vertical, false) => "top",
            (Self::Horizontal, true) => "right",
            (Self::Horizontal, false) => "left",
        }
    }

    /// The `AXScrollArea` attribute holding this axis's scroll bar.
    pub fn bar_attribute(self) -> &'static str {
        match self {
            Self::Vertical => "AXVerticalScrollBar",
            Self::Horizontal => "AXHorizontalScrollBar",
        }
    }

    /// The delta field for this axis. `dy` is vertical, `dx` is horizontal.
    pub fn delta(self, dx: i32, dy: i32) -> i32 {
        match self {
            Self::Vertical => dy,
            Self::Horizontal => dx,
        }
    }

    /// Every axis, in the order a scroll reports them: vertical first.
    pub fn both() -> [Self; 2] {
        [Self::Vertical, Self::Horizontal]
    }
}

/// Which mechanism produced the scroll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// The posted wheel event moved the scroll bar.
    Event,
    /// `AXValue` was written on the scroll bar.
    AxScrollbar,
}

impl Via {
    /// Stable JSON name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::AxScrollbar => "ax_scrollbar",
        }
    }
}

/// What comparing the bar before and after the wheel event means.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Decision {
    /// The wheel event moved the bar. `before` and `after` are its values.
    Event { before: f64, after: f64 },
    /// The bar did not move and is not at its limit: write `AXValue`.
    AxScrollbar { before: f64, next: f64 },
    /// The bar did not move because it is already at its limit.
    AtLimit { value: f64 },
    /// No scroll bar could be read, so nothing was verified.
    Unverifiable,
}

/// True when the two readings differ by more than [`EPSILON`].
pub fn moved(before: f64, after: f64) -> bool {
    (after - before).abs() > EPSILON
}

/// True when a bar at `value` cannot scroll `steps` further.
///
/// A zero step is never at a limit: it asks for no movement.
pub fn at_limit(value: f64, steps: i32) -> bool {
    match steps.cmp(&0) {
        std::cmp::Ordering::Greater => value >= 1.0 - EPSILON,
        std::cmp::Ordering::Less => value <= EPSILON,
        std::cmp::Ordering::Equal => false,
    }
}

/// The per-line fraction (and the warnings from sizing it) the AX scroll-bar
/// fallback uses.
///
/// `value` is in `0..=1` and means "this much of the bar a wheel line moves."
/// `warnings` carries [`APPROXIMATED_WARNING`] when the size data was missing
/// and [`CONTENT_FITS_WARNING`] when the document already fits inside the
/// visible viewport. A clean measurement has no warnings.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// Per-line fraction of the bar (0..=1).
    pub value: f64,
    /// Non-fatal sizing notes. Empty on a clean measurement.
    pub warnings: Vec<String>,
}

impl Step {
    /// A measured step with no warnings.
    pub const fn measured(value: f64) -> Self {
        Self {
            value,
            warnings: Vec::new(),
        }
    }

    /// The fallback step the legacy `step_per_line(None)` produced: five
    /// percent, no warnings. Used by callers that have no sizing data and
    /// want the same step the old approximation delivered.
    pub const fn approx() -> Self {
        Self::measured(DEFAULT_STEP)
    }
}

/// The per-line scroll step computed from the document content's size along
/// the scroll axis and the visible viewport's size along the same axis.
///
/// `content` is the size of the scroll area's document child — the first
/// `AXChildren` element that is not an `AXScrollBar` (typically
/// `AXTextArea`, `AXTable`, `AXOutline`, or `AXGroup`). `visible` is the
/// scroll area's own `AXSize` along the axis, minus the orthogonal scroll
/// bar's height (or width) when both bars are present.
///
/// One wheel notch is [`LINE_POINTS`] points (≈ three text lines), so one
/// wheel line moves `LINE_POINTS / (content - visible)` of the bar, clamped
/// to `0..=1`. The unit tests pin every branch:
///
/// - `Some(2400), Some(400)` → `0.02` so ten wheel lines move `0.2` of the bar.
/// - `content <= visible` → `0.0` with [`CONTENT_FITS_WARNING`] (nothing to
///   scroll).
/// - missing or non-finite sizes → [`DEFAULT_STEP`] with
///   [`APPROXIMATED_WARNING`] (we did not measure, so the step is a guess).
pub fn content_step(content: Option<f64>, visible: Option<f64>) -> Step {
    match (content, visible) {
        (Some(c), Some(v)) if c.is_finite() && v.is_finite() && v > 0.0 && c > v => {
            Step::measured((LINE_POINTS / (c - v)).clamp(0.0, 1.0))
        }
        (Some(c), Some(v)) if c.is_finite() && v.is_finite() && v > 0.0 => Step {
            value: 0.0,
            warnings: vec![CONTENT_FITS_WARNING.to_string()],
        },
        _ => Step {
            value: DEFAULT_STEP,
            warnings: vec![APPROXIMATED_WARNING.to_string()],
        },
    }
}

/// Signed value delta for `steps` wheel lines using `step`. Positive is down or
/// right.
pub fn ax_delta(steps: i32, step: &Step) -> f64 {
    f64::from(steps) * step.value
}

/// The clamped value to write for `steps` wheel lines from `before`.
pub fn next_value(before: f64, steps: i32, step: &Step) -> f64 {
    (before + ax_delta(steps, step)).clamp(0.0, 1.0)
}

/// Decide what the measurement means.
///
/// `before` and `after` are the scroll-bar values around the wheel event, or
/// `None` when the bar could not be read. `step` is the sized per-line
/// fraction; the caller passes the same `Step` from the read pass, so the
/// `AxScrollbar` decision uses the same math that produced the warning. The
/// order matters: a bar that moved wins over a bar that sits at its limit,
/// because a bar sitting at its limit cannot have moved.
pub fn decide(before: Option<f64>, after: Option<f64>, steps: i32, step: &Step) -> Decision {
    let (Some(before), Some(after)) = (before, after) else {
        return Decision::Unverifiable;
    };
    if !before.is_finite() || !after.is_finite() {
        return Decision::Unverifiable;
    }
    if moved(before, after) {
        return Decision::Event { before, after };
    }
    if at_limit(before, steps) {
        return Decision::AtLimit { value: before };
    }
    Decision::AxScrollbar {
        before,
        next: next_value(before, steps, step),
    }
}

/// What a scroll actually did, in the shape the response carries.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// Which mechanism produced the scroll.
    pub via: Via,
    /// True only when a scroll-bar value changed.
    pub scrolled: bool,
    /// Bar value before the scroll, when one could be read.
    pub before: Option<f64>,
    /// Bar value after the scroll, when one could be read.
    pub after: Option<f64>,
    /// Per-line fraction (0..=1) used for the `AXValue` write, or the value
    /// we observed before honouring the event tier. `None` when the result
    /// came back unreadable. Surfaced in the response for transparency.
    pub step: Option<f64>,
    /// Non-fatal notes. [`Outcome::warnings`] is empty on a clean event scroll.
    pub warnings: Vec<String>,
}

impl Outcome {
    /// The wheel event moved the bar.
    pub fn event(before: f64, after: f64, step: f64) -> Self {
        Self {
            via: Via::Event,
            scrolled: moved(before, after),
            before: Some(before),
            after: Some(after),
            step: Some(step),
            warnings: Vec::new(),
        }
    }

    /// `AXValue` was written. `read_back` is the value re-read after the write.
    pub fn ax_scrollbar(
        before: f64,
        read_back: Option<f64>,
        step: f64,
        warnings: Vec<String>,
    ) -> Self {
        let scrolled = read_back.is_some_and(|value| moved(before, value));
        Self {
            via: Via::AxScrollbar,
            scrolled,
            before: Some(before),
            after: read_back,
            step: Some(step),
            warnings,
        }
    }

    /// The bar is already at its limit, so nothing moved and nothing is wrong.
    pub fn at_limit(axis: Axis, steps: i32, value: f64, step: Option<f64>) -> Self {
        Self {
            via: Via::Event,
            scrolled: false,
            before: Some(value),
            after: Some(value),
            step,
            warnings: vec![format!(
                "{} scroll bar is already at the {} limit",
                axis.as_str(),
                axis.limit(steps)
            )],
        }
    }

    /// The caller asked for no movement, as `scroll --dx 0 --dy 0` does.
    ///
    /// No axis is addressed, so there is no bar to report a value for. This is
    /// `scrolled: false` with no warning: nothing was attempted and nothing
    /// failed.
    pub fn none() -> Self {
        Self {
            via: Via::Event,
            scrolled: false,
            before: None,
            after: None,
            step: None,
            warnings: Vec::new(),
        }
    }
}

/// Why an AX scroll-bar fallback could not finish.
///
/// The backend turns the reason into `execution_failed`; nothing is reported
/// as a scroll that did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxFailure {
    /// The scroll bar refused the `AXValue` write.
    Refused,
    /// The value was written but reading it back did not confirm a change.
    Unconfirmed,
}

impl AxFailure {
    /// Reason text for the error message.
    pub fn reason(self, axis: Axis) -> String {
        match self {
            Self::Refused => format!("AX refused the {} scroll bar value write", axis.as_str()),
            Self::Unconfirmed => format!(
                "the {} scroll bar value did not change after AX wrote it",
                axis.as_str()
            ),
        }
    }
}

/// Why the AX scroll-bar path is running.
///
/// This decides the warning. Naming the cause keeps the response honest: a
/// caller who asked for `--tier ax` never posted a wheel event, so the
/// "wheel event not delivered" note would be false for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxCause {
    /// A wheel event was posted first and did not move the bar.
    WheelEventNotDelivered,
    /// The caller named `--tier ax`, so the scroll bar was the plan all along.
    ExplicitAxTier,
}

impl AxCause {
    /// The warning this cause earns. An explicitly requested AX scroll needs none.
    pub fn warning(self) -> Option<&'static str> {
        match self {
            Self::WheelEventNotDelivered => Some(AX_FALLBACK_WARNING),
            Self::ExplicitAxTier => None,
        }
    }
}

/// The outcome of writing `AXValue`, or the reason it failed.
///
/// `requested` is the value [`Decision::AxScrollbar`] asked for and
/// `read_back` is what the bar reports afterwards. A bar that reads back a
/// different value than requested still counts as scrolled: the app may round
/// to its own page, and that note is added as a second warning. The `step`'s
/// own warnings are merged in, so a measurement that approximated or hit a
/// `content fits` document surfaces in the response alongside the AX-fallback
/// notes.
pub fn ax_result(
    axis: Axis,
    before: f64,
    requested: f64,
    read_back: Option<f64>,
    cause: AxCause,
    step: &Step,
) -> Result<Outcome, AxFailure> {
    let scrolled = read_back.is_some_and(|value| moved(before, value));
    if !scrolled {
        return Err(AxFailure::Unconfirmed);
    }
    let mut warnings = step.warnings.clone();
    if let Some(text) = cause.warning() {
        warnings.push(text.to_string());
    }
    if let Some(value) = read_back {
        if (value - requested).abs() > ROUNDING_TOLERANCE {
            warnings.push(format!(
                "{} scroll bar landed on {value:.3} instead of the requested {requested:.3}",
                axis.as_str()
            ));
        }
    }
    Ok(Outcome::ax_scrollbar(
        before, read_back, step.value, warnings,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_event_that_moved_the_bar_wins_over_everything() {
        let step = Step::measured(0.2);
        assert_eq!(
            decide(Some(0.0), Some(0.2), 3, &step),
            Decision::Event {
                before: 0.0,
                after: 0.2
            }
        );
        // Upward: a negative delta lowers the value.
        assert_eq!(
            decide(Some(0.9), Some(0.7), -3, &step),
            Decision::Event {
                before: 0.9,
                after: 0.7
            }
        );
    }

    #[test]
    fn an_unmoved_bar_outside_its_limit_asks_for_the_ax_write() {
        let step = Step::measured(0.1);
        let Decision::AxScrollbar { before, next } = decide(Some(0.0), Some(0.0), 3, &step) else {
            panic!("expected the AX scroll bar path");
        };
        assert_eq!(before, 0.0);
        // 0.1 per line × 3 lines = 0.3
        assert!((next - 0.3).abs() < 1e-9, "next was {next}");
    }

    #[test]
    fn an_unmoved_bar_at_its_limit_is_not_a_fallback() {
        let step = Step::measured(0.2);
        assert_eq!(
            decide(Some(1.0), Some(1.0), 3, &step),
            Decision::AtLimit { value: 1.0 }
        );
        assert_eq!(
            decide(Some(0.0), Some(0.0), -3, &step),
            Decision::AtLimit { value: 0.0 }
        );
        // A bar at the bottom is not at its limit when scrolling up.
        assert!(matches!(
            decide(Some(1.0), Some(1.0), -3, &step),
            Decision::AxScrollbar { .. }
        ));
        // A bar at the top is not at its limit when scrolling down.
        assert!(matches!(
            decide(Some(0.0), Some(0.0), 3, &step),
            Decision::AxScrollbar { .. }
        ));
    }

    #[test]
    fn a_missing_or_unreadable_bar_is_unverifiable() {
        let step = Step::approx();
        assert_eq!(decide(None, Some(0.2), 3, &step), Decision::Unverifiable);
        assert_eq!(decide(Some(0.0), None, 3, &step), Decision::Unverifiable);
        assert_eq!(decide(None, None, 3, &step), Decision::Unverifiable);
        assert_eq!(
            decide(Some(f64::NAN), Some(0.2), 3, &step),
            Decision::Unverifiable
        );
        assert_eq!(
            decide(Some(f64::INFINITY), Some(0.2), 3, &step),
            Decision::Unverifiable
        );
    }

    #[test]
    fn a_rounding_wobble_is_not_a_scroll() {
        assert!(!moved(0.2, 0.2 + EPSILON / 2.0));
        assert!(moved(0.2, 0.2 + EPSILON * 10.0));
    }

    #[test]
    fn content_step_uses_line_points_over_the_content_overflow() {
        // The README example: content 2400, visible 400 → 40 / (2400-400) = 0.02.
        let step = content_step(Some(2400.0), Some(400.0));
        assert!((step.value - 0.02).abs() < 1e-9, "step was {}", step.value);
        assert!(step.warnings.is_empty(), "{:?}", step.warnings);

        // A scroll area where the document fills 16% of the viewport.
        let step = content_step(Some(1160.0), Some(1000.0));
        // LINE_POINTS / 160 = 0.25
        assert!((step.value - 0.25).abs() < 1e-9, "step was {}", step.value);
    }

    #[test]
    fn content_step_clamps_into_zero_to_one() {
        // A document that is so close to the viewport that LINE_POINTS would
        // exceed 1.0 — clamp so one wheel line never jumps the whole bar.
        let step = content_step(Some(1040.0), Some(1000.0));
        assert!((step.value - 1.0).abs() < 1e-9, "step was {}", step.value);
        assert!(step.warnings.is_empty());
        assert!(step.value >= 0.0 && step.value <= 1.0);

        // A tiny overflow is still positive: 40 / 40 = 1.0, the upper clamp.
        let step = content_step(Some(10_001.0), Some(10_000.0));
        assert!(step.value > 0.0, "step was {}", step.value);
        assert!(step.value <= 1.0);
    }

    #[test]
    fn content_step_says_content_fits_when_there_is_nothing_to_scroll() {
        // Strict equality: document == viewport.
        let step = content_step(Some(800.0), Some(800.0));
        assert_eq!(step.value, 0.0);
        assert_eq!(step.warnings, vec![CONTENT_FITS_WARNING.to_string()]);

        // Document smaller than viewport (oversized scroll area).
        let step = content_step(Some(400.0), Some(800.0));
        assert_eq!(step.value, 0.0);
        assert_eq!(step.warnings, vec![CONTENT_FITS_WARNING.to_string()]);
    }

    #[test]
    fn content_step_falls_back_to_a_five_percent_step_when_sizing_is_missing() {
        // No content and no visible → approximated, DEFAULT_STEP, warning.
        let step = content_step(None, None);
        assert_eq!(step.value, DEFAULT_STEP);
        assert_eq!(step.warnings, vec![APPROXIMATED_WARNING.to_string()]);

        // Half-sized → still missing the other side.
        assert_eq!(
            content_step(Some(1200.0), None).warnings,
            vec![APPROXIMATED_WARNING.to_string()]
        );
        assert_eq!(
            content_step(None, Some(800.0)).warnings,
            vec![APPROXIMATED_WARNING.to_string()]
        );

        // Non-finite or non-positive sizes follow the same fallback path.
        assert_eq!(
            content_step(Some(f64::NAN), Some(800.0)).value,
            DEFAULT_STEP
        );
        assert_eq!(
            content_step(Some(1200.0), Some(0.0)).warnings,
            vec![APPROXIMATED_WARNING.to_string()]
        );
        assert_eq!(
            content_step(Some(1200.0), Some(-10.0)).warnings,
            vec![APPROXIMATED_WARNING.to_string()]
        );
    }

    #[test]
    fn content_step_drives_next_value_for_ten_wheel_lines() {
        let step = content_step(Some(2400.0), Some(400.0));
        // 10 lines × 0.02 = 0.20
        assert!((next_value(0.0, 10, &step) - 0.20).abs() < 1e-9);
        assert!((next_value(0.5, -10, &step) - 0.30).abs() < 1e-9);
        // Past the end clamps.
        assert_eq!(next_value(0.95, 10, &step), 1.0);
        assert_eq!(next_value(0.05, -10, &step), 0.0);
    }

    #[test]
    fn step_approx_keeps_the_legacy_default_with_no_warnings() {
        let step = Step::approx();
        assert_eq!(step.value, DEFAULT_STEP);
        assert!(step.warnings.is_empty());
    }

    #[test]
    fn ax_delta_multiplies_steps_by_the_step_value() {
        let step = content_step(Some(2400.0), Some(400.0));
        assert!((ax_delta(10, &step) - 0.20).abs() < 1e-9);
        assert!((ax_delta(-10, &step) + 0.20).abs() < 1e-9);
        assert_eq!(ax_delta(0, &step), 0.0);

        // The fallback step gives the legacy five-percent value.
        let step = Step::approx();
        assert!((ax_delta(3, &step) - 0.15).abs() < 1e-9);
    }

    #[test]
    fn the_written_value_is_clamped_into_zero_to_one() {
        let step = Step::approx();
        assert_eq!(next_value(0.0, -5, &step), 0.0);
        assert_eq!(next_value(1.0, 5, &step), 1.0);
        assert!((next_value(0.5, 2, &step) - 0.6).abs() < 1e-9);
        assert!((next_value(0.5, -2, &step) - 0.4).abs() < 1e-9);
    }

    #[test]
    fn at_limit_only_answers_for_a_nonzero_step_in_that_direction() {
        assert!(at_limit(1.0, 1));
        assert!(at_limit(1.0 - EPSILON / 2.0, 1));
        assert!(!at_limit(1.0, -1));
        assert!(at_limit(0.0, -1));
        // Within one epsilon of the edge counts as at the limit.
        assert!(at_limit(EPSILON / 2.0, -1));
        assert!(!at_limit(0.999_9, 1));
        assert!(!at_limit(0.000_1, -1));
        // A zero step asks for no movement, so it is never "at the limit".
        assert!(!at_limit(1.0, 0));
        assert!(!at_limit(0.0, 0));
    }

    #[test]
    fn an_event_outcome_reports_a_move_only_when_the_value_changed() {
        let moved_outcome = Outcome::event(0.0, 0.2, 0.05);
        assert_eq!(moved_outcome.via, Via::Event);
        assert!(moved_outcome.scrolled);
        assert_eq!(moved_outcome.before, Some(0.0));
        assert_eq!(moved_outcome.after, Some(0.2));
        assert_eq!(moved_outcome.step, Some(0.05));
        assert!(moved_outcome.warnings.is_empty());
        assert!(!Outcome::event(0.5, 0.5, 0.05).scrolled);
        assert_eq!(Outcome::event(0.5, 0.5, 0.05).step, Some(0.05));
    }

    #[test]
    fn an_ax_outcome_carries_the_fallback_warning_and_step() {
        let outcome =
            Outcome::ax_scrollbar(0.0, Some(0.3), 0.1, vec![AX_FALLBACK_WARNING.to_string()]);
        assert_eq!(outcome.via, Via::AxScrollbar);
        assert!(outcome.scrolled);
        assert_eq!(outcome.after, Some(0.3));
        assert_eq!(outcome.step, Some(0.1));
        assert_eq!(outcome.warnings, vec![AX_FALLBACK_WARNING.to_string()]);
        assert_eq!(Via::AxScrollbar.as_str(), "ax_scrollbar");
        assert_eq!(Via::Event.as_str(), "event");
    }

    #[test]
    fn at_limit_says_so_without_claiming_a_scroll() {
        let outcome = Outcome::at_limit(Axis::Vertical, 3, 1.0, Some(0.05));
        assert!(!outcome.scrolled);
        assert_eq!(outcome.before, Some(1.0));
        assert_eq!(outcome.after, Some(1.0));
        assert_eq!(outcome.step, Some(0.05));
        assert_eq!(outcome.warnings.len(), 1);
        assert!(
            outcome.warnings[0].contains("bottom"),
            "{:?}",
            outcome.warnings
        );
        assert!(
            Outcome::at_limit(Axis::Horizontal, -2, 0.0, Some(0.05)).warnings[0].contains("left")
        );
        assert_eq!(Axis::Vertical.bar_attribute(), "AXVerticalScrollBar");
        assert_eq!(Axis::Horizontal.bar_attribute(), "AXHorizontalScrollBar");
    }

    #[test]
    fn an_unconfirmed_ax_write_is_a_failure_not_a_scroll() {
        let step = Step::approx();
        assert_eq!(
            ax_result(
                Axis::Vertical,
                0.0,
                0.3,
                Some(0.0),
                AxCause::WheelEventNotDelivered,
                &step,
            ),
            Err(AxFailure::Unconfirmed)
        );
        assert_eq!(
            ax_result(
                Axis::Vertical,
                0.0,
                0.3,
                None,
                AxCause::WheelEventNotDelivered,
                &step,
            ),
            Err(AxFailure::Unconfirmed)
        );
        assert_eq!(
            AxFailure::Refused.reason(Axis::Horizontal),
            "AX refused the horizontal scroll bar value write"
        );
        assert!(AxFailure::Unconfirmed
            .reason(Axis::Vertical)
            .contains("did not change"));
    }

    #[test]
    fn an_app_rounding_to_its_own_page_still_counts_as_scrolled() {
        let step = Step::measured(0.1);
        let outcome = ax_result(
            Axis::Vertical,
            0.0,
            0.3,
            Some(0.25),
            AxCause::WheelEventNotDelivered,
            &step,
        )
        .expect("scrolled");
        assert!(outcome.scrolled);
        assert_eq!(outcome.via, Via::AxScrollbar);
        assert_eq!(outcome.after, Some(0.25));
        assert_eq!(outcome.step, Some(0.1));
        assert_eq!(outcome.warnings[0], AX_FALLBACK_WARNING);
        assert!(
            outcome.warnings[1].contains("0.250") && outcome.warnings[1].contains("0.300"),
            "{:?}",
            outcome.warnings
        );
    }

    #[test]
    fn ax_result_merges_step_warnings_alongside_the_fallback_note() {
        // A measured step that was approximated: the response surfaces the
        // step's note first, then the AX fallback note. read_back matches the
        // request so no rounding warning is added.
        let step = content_step(None, None);
        let outcome = ax_result(
            Axis::Vertical,
            0.0,
            0.15,
            Some(0.15),
            AxCause::WheelEventNotDelivered,
            &step,
        )
        .expect("scrolled");
        assert_eq!(outcome.step, Some(DEFAULT_STEP));
        assert_eq!(
            outcome.warnings,
            vec![
                APPROXIMATED_WARNING.to_string(),
                AX_FALLBACK_WARNING.to_string()
            ]
        );

        // A content-fits step on the explicit AX tier with the bar already at
        // zero: ax_result reports Unconfirmed for a write that did not move
        // the bar, regardless of how clean the step was.
        let step = content_step(Some(800.0), Some(800.0));
        let outcome = ax_result(
            Axis::Vertical,
            0.0,
            0.0,
            Some(0.0),
            AxCause::ExplicitAxTier,
            &step,
        );
        assert_eq!(outcome, Err(AxFailure::Unconfirmed));

        // A content-fits step that moves the bar carries the step's warning
        // but not the AX fallback note (explicit `--tier ax`).
        let step = content_step(Some(800.0), Some(800.0));
        let outcome = ax_result(
            Axis::Vertical,
            0.0,
            0.0,
            Some(0.001),
            AxCause::ExplicitAxTier,
            &step,
        )
        .expect("scrolled");
        assert_eq!(outcome.step, Some(0.0));
        assert_eq!(outcome.warnings, vec![CONTENT_FITS_WARNING.to_string()]);
    }

    #[test]
    fn axes_pick_their_own_delta_and_direction() {
        assert_eq!(Axis::Vertical.delta(3, 7), 7);
        assert_eq!(Axis::Horizontal.delta(3, 7), 3);
        assert_eq!(Axis::Vertical.limit(1), "bottom");
        assert_eq!(Axis::Vertical.limit(-1), "top");
        assert_eq!(Axis::Horizontal.limit(0), "right");
        assert_eq!(Axis::both(), [Axis::Vertical, Axis::Horizontal]);
        assert_eq!(Axis::Vertical.as_str(), "vertical");
    }

    #[test]
    fn an_explicit_ax_scroll_never_claims_a_wheel_event_was_dropped() {
        // The caller asked for the scroll bar, so no wheel event was posted and
        // the "not delivered" note would be false.
        let step = Step::measured(0.1);
        let outcome = ax_result(
            Axis::Vertical,
            0.0,
            0.3,
            Some(0.3),
            AxCause::ExplicitAxTier,
            &step,
        )
        .expect("scrolled");
        assert!(outcome.scrolled);
        assert_eq!(outcome.via, Via::AxScrollbar);
        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
        assert_eq!(AxCause::ExplicitAxTier.warning(), None);
        assert_eq!(
            AxCause::WheelEventNotDelivered.warning(),
            Some(AX_FALLBACK_WARNING)
        );
        // A refusal is still a refusal whoever asked for the scroll bar.
        assert_eq!(
            ax_result(
                Axis::Vertical,
                0.0,
                0.3,
                Some(0.0),
                AxCause::ExplicitAxTier,
                &step,
            ),
            Err(AxFailure::Unconfirmed)
        );
    }

    #[test]
    fn a_zero_delta_scroll_is_not_a_scroll_and_not_a_failure() {
        let outcome = Outcome::none();
        assert!(!outcome.scrolled);
        assert_eq!(outcome.before, None);
        assert_eq!(outcome.after, None);
        assert_eq!(outcome.step, None);
        assert!(outcome.warnings.is_empty());
        // No axis is addressed, so no bar exists to report and no limit applies.
        assert!(!at_limit(0.0, 0));
        assert!(!at_limit(1.0, 0));
        let step = Step::measured(0.5);
        assert_eq!(ax_delta(0, &step), 0.0);
        assert_eq!(next_value(0.4, 0, &step), 0.4);
    }

    #[test]
    fn the_measured_fallback_value_matches_the_decision() {
        let step = Step::measured(0.3);
        let Decision::AxScrollbar { before, next } = decide(Some(0.4), Some(0.4), 2, &step) else {
            panic!("expected the AX scroll bar path");
        };
        assert_eq!(next, next_value(before, 2, &step));
        let outcome = ax_result(
            Axis::Vertical,
            before,
            next,
            Some(next),
            AxCause::WheelEventNotDelivered,
            &step,
        )
        .expect("scrolled");
        assert!(outcome.scrolled);
        assert_eq!(outcome.after, Some(next));
        assert_eq!(outcome.step, Some(0.3));
    }
}

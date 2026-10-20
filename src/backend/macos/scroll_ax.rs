//! Find and drive an `AXScrollArea` scroll bar so a background scroll can be
//! verified and, when needed, performed without activating the app.
//!
//! A scroll wheel posted with `CGEventPostToPid` is not delivered to the views
//! of a non-frontmost app: the post returns without error and nothing moves.
//! Writing `AXValue` on the scroll bar does move it, needs no activation, and
//! does not move the cursor. This module owns the Accessibility half of that:
//! locating the scroll area, reading and writing the bar, and measuring the
//! content-over-visible sizes that turn wheel lines into a value delta.
//!
//! The decision logic lives in [`crate::scroll`], which is pure and tested on
//! every OS. This file only measures and acts.

#![allow(unsafe_code)]
// The callers live behind `#[cfg(not(test))]` in `input.rs`, so a unit-test
// build sees this module as unused. The live build uses every item here.
#![cfg_attr(test, allow(dead_code))]

use std::time::{Duration, Instant};

use objc2_application_services::AXUIElement;
use objc2_core_foundation::CFRetained;

use super::ax::{self, AttrError, WalkError};

/// Cap on the parents walked up from a hit-tested element.
const PARENT_CAP: usize = 24;

/// Cap on children visited while looking for a scroll area.
const SCROLL_AREA_CHILD_CAP: usize = 64;

/// Depth cap for the window-wide scroll-area search.
const SEARCH_DEPTH: usize = 12;

/// Wall-clock budget for one lookup.
const SEARCH_BUDGET: Duration = Duration::from_secs(2);

/// Settle time between reading the bar before the post and reading it after.
///
/// A wheel event is asynchronous, and the app may run its scroll on the next
/// turn of the run loop. 150 ms is long enough for that on a busy machine and
/// short enough that a caller is not left waiting on every scroll.
pub(crate) const SETTLE: Duration = Duration::from_millis(150);

/// Cap on how many `AXChildren` we walk looking for the document child. A
/// scroll area typically has only one document child, but real apps sometimes
/// hide helpers, toolbars, or overlays as siblings; we stop early to keep the
/// measurement bounded.
const CONTENT_CHILD_SCAN_CAP: usize = 8;

/// The scroll bar for one axis of a scroll area.
///
/// `bar` is retained on its own, so it stays valid without holding the scroll
/// area alive; `step` is measured once, while both are to hand.
pub(crate) struct ScrollBar {
    bar: CFRetained<AXUIElement>,
    /// Per-line scroll step, in `0..=1`, derived from the content-over-visible
    /// sizes. Carries the warning the sizing pass produced, when applicable.
    step: crate::scroll::Step,
}

impl ScrollBar {
    /// Read `AXValue` on the bar.
    pub(crate) fn value(&self) -> Result<Option<f64>, WalkError> {
        ax::copy_f64(&self.bar, "AXValue").map_err(from_attr)
    }

    /// Write `AXValue` on the bar.
    pub(crate) fn set_value(&self, value: f64) -> Result<(), WalkError> {
        ax::set_f64(&self.bar, "AXValue", value)
    }

    /// The per-line scroll step (0..=1) the AX scroll-bar write should use.
    pub(crate) fn step(&self) -> &crate::scroll::Step {
        &self.step
    }
}

/// A `kAXErrorAttributeUnsupported` read is a missing bar, not a failure.
fn from_attr(error: AttrError) -> WalkError {
    match error {
        AttrError::Permission => WalkError::Permission(permission_message()),
        AttrError::Timeout => WalkError::Timeout,
        AttrError::Failed => WalkError::Message("AX scroll bar read failed".into()),
        AttrError::Missing => WalkError::Message("AX scroll bar attribute is absent".into()),
    }
}

fn permission_message() -> String {
    ax::permission_message_for(
        &super::apps::responsible_app().map_or_else(|| "computer-use".to_string(), |app| app.name),
    )
}

/// The scroll bar for `axis` under `(x, y)`, or the window's largest one.
///
/// A hit test is preferred because a window can hold several scroll areas and
/// the caller named a point. When there is no point, or the point is not over
/// anything scrollable, the first scroll area with a usable bar wins, then the
/// largest by area size.
pub(crate) fn find(
    app: &AXUIElement,
    point: (Option<i32>, Option<i32>),
    axis: crate::scroll::Axis,
) -> Result<Option<ScrollBar>, WalkError> {
    ax::ensure_trusted()?;
    if let (Some(x), Some(y)) = point {
        if let Some(bar) = scroll_area_at(app, x, y, axis)? {
            return Ok(Some(bar));
        }
    }
    largest_scroll_area(app, axis)
}

/// Walk up from the element at `(x, y)` until a scroll area is found.
fn scroll_area_at(
    app: &AXUIElement,
    x: i32,
    y: i32,
    axis: crate::scroll::Axis,
) -> Result<Option<ScrollBar>, WalkError> {
    let Some(mut element) = element_at(app, x, y)? else {
        return Ok(None);
    };
    for _ in 0..=PARENT_CAP {
        if let Some(bar) = scroll_bar(&element, axis) {
            return Ok(Some(bar));
        }
        match ax::parent(&element) {
            Some(parent) => element = parent,
            None => return Ok(None),
        }
    }
    Ok(None)
}

/// The first scroll area in the app that has a usable bar for `axis`.
///
/// The first hit keeps the cost bounded: an app's own scroll area comes
/// before a helper window's, and a second scroll area is rarely what a caller
/// means when it named no point.
fn largest_scroll_area(
    app: &AXUIElement,
    axis: crate::scroll::Axis,
) -> Result<Option<ScrollBar>, WalkError> {
    let deadline = Instant::now() + SEARCH_BUDGET;
    let mut best: Option<(f64, ScrollBar)> = None;
    search(app, axis, 0, &mut best, &mut Vec::new(), deadline)?;
    Ok(best.map(|(_, bar)| bar))
}

fn search(
    element: &AXUIElement,
    axis: crate::scroll::Axis,
    depth: usize,
    best: &mut Option<(f64, ScrollBar)>,
    seen: &mut Vec<usize>,
    deadline: Instant,
) -> Result<(), WalkError> {
    if Instant::now() >= deadline || depth >= SEARCH_DEPTH {
        return Ok(());
    }
    if let Some(bar) = scroll_bar(element, axis) {
        let area = size_of(element).unwrap_or(0.0);
        // The first candidate wins unless a later one is clearly larger, which
        // keeps "first scroll area" the common answer without a full walk.
        if best
            .as_ref()
            .is_none_or(|(best_area, _)| area > *best_area * 1.5)
        {
            *best = Some((area, bar));
        }
        // A scroll area's own scroll bar is the answer; its children are content.
        return Ok(());
    }
    let pointer = std::ptr::from_ref(element) as usize;
    if seen.contains(&pointer) {
        return Ok(());
    }
    seen.push(pointer);
    let children = ax::children(element, SCROLL_AREA_CHILD_CAP, deadline).unwrap_or_default();
    for child in children {
        if Instant::now() >= deadline {
            return Ok(());
        }
        search(&child, axis, depth + 1, best, seen, deadline)?;
    }
    Ok(())
}

/// The scroll bar `axis` advertises on `element`, with its sized step.
fn scroll_bar(element: &AXUIElement, axis: crate::scroll::Axis) -> Option<ScrollBar> {
    if role(element).as_deref() != Some("AXScrollArea") {
        return None;
    }
    let bar = ax::copy_cf(element, axis.bar_attribute())
        .ok()
        .flatten()
        .and_then(|value| value.downcast::<AXUIElement>().ok())?;
    let step = measure_step(element, &bar, axis);
    Some(ScrollBar { bar, step })
}

/// Build the per-line scroll step for `axis`.
///
/// `visible` is the scroll area's own `AXSize` along `axis`, minus the
/// orthogonal scroll bar (a horizontal bar inside a vertical scroll area eats
/// some vertical space, and vice versa) when both are present. `content` is
/// the `AXSize` along `axis` of the first `AXChildren` that is not another
/// scroll bar — typically `AXTextArea`, `AXTable`, `AXOutline`, or `AXGroup`.
/// Anything missing here falls through [`crate::scroll::content_step`], which
/// advertises [`crate::scroll::APPROXIMATED_WARNING`] on the response.
fn measure_step(
    area: &AXUIElement,
    bar: &AXUIElement,
    axis: crate::scroll::Axis,
) -> crate::scroll::Step {
    let visible_area = ax::ax_size_of(area);
    let visible = match (visible_area, axis) {
        (Some((_, height)), crate::scroll::Axis::Vertical) => {
            let subtract =
                orthogonal_bar_length(area, bar, crate::scroll::Axis::Horizontal).unwrap_or(0.0);
            Some(height - subtract)
        }
        (Some((width, _)), crate::scroll::Axis::Horizontal) => {
            let subtract =
                orthogonal_bar_length(area, bar, crate::scroll::Axis::Vertical).unwrap_or(0.0);
            Some(width - subtract)
        }
        (None, _) => None,
    };
    let content = content_size_for_axis(area, axis);
    crate::scroll::content_step(content, visible)
}

/// Length of the orthogonal scroll bar, when both bars are present.
///
/// We compare each child of the scroll area against the scroll-bar attribute
/// names — `AXVerticalScrollBar` and `AXHorizontalScrollBar`. Anything
/// matching is a scroll bar; everything else is a candidate for content. The
/// comparison is generous (a child's role may be hidden) so we fall back to
/// the scroll bar itself rather than misclassifying it as document content.
fn orthogonal_bar_length(
    area: &AXUIElement,
    bar: &AXUIElement,
    orthogonal_axis: crate::scroll::Axis,
) -> Option<f64> {
    if !has_orthogonal_bar(area, orthogonal_axis) {
        return None;
    }
    size_along(bar, orthogonal_axis)
}

/// True when `area` advertises a scroll bar along `orthogonal_axis`.
///
/// Same-source check: when `area` has the `AXVerticalScrollBar` attribute the
/// caller did not ask about, that bar shares `area` and its length along the
/// orthogonal axis is the amount to subtract from `visible`.
fn has_orthogonal_bar(area: &AXUIElement, orthogonal_axis: crate::scroll::Axis) -> bool {
    matches!(
        ax::copy_cf(area, orthogonal_axis.bar_attribute()),
        Ok(Some(_))
    )
}

/// `AXSize` along `axis` for the first `AXChildren` element of `area` that is
/// not an `AXScrollBar`.
///
/// AX exposes document content as a single scroll child (the `AXTextArea`,
/// `AXTable`, `AXOutline`, or `AXGroup`). The bar of the *other* axis is also
/// a child, but its role is hidden; we still skip it by looking for the scroll
/// bar attribute matches.
fn content_size_for_axis(area: &AXUIElement, axis: crate::scroll::Axis) -> Option<f64> {
    let deadline = Instant::now() + Duration::from_millis(250);
    let children = ax::children(area, CONTENT_CHILD_SCAN_CAP, deadline).ok()?;
    let bar_attributes = [
        crate::scroll::Axis::Horizontal.bar_attribute(),
        crate::scroll::Axis::Vertical.bar_attribute(),
    ];
    for child in children {
        if is_scroll_bar(&child, &bar_attributes) {
            continue;
        }
        if let Some(size) = size_along(&child, axis) {
            if size > 0.0 {
                return Some(size);
            }
        }
    }
    None
}

/// True when `child` is one of the named scroll bars on `area`.
///
/// We compare against `AXValue`-less attribute copies; a `None` means the
/// scroll bar attribute is not set on this child, so the child is the
/// document rather than a bar.
fn is_scroll_bar(child: &AXUIElement, bar_attributes: &[&str]) -> bool {
    bar_attributes
        .iter()
        .any(|attribute| matches!(ax::copy_cf(child, attribute), Ok(Some(_))))
}

fn size_of(element: &AXUIElement) -> Option<f64> {
    let (width, height) = ax::ax_size_of(element)?;
    Some(width * height)
}

fn size_along(element: &AXUIElement, axis: crate::scroll::Axis) -> Option<f64> {
    let (width, height) = ax::ax_size_of(element)?;
    match axis {
        crate::scroll::Axis::Vertical => Some(height),
        crate::scroll::Axis::Horizontal => Some(width),
    }
}

fn role(element: &AXUIElement) -> Option<String> {
    ax::copy_string(element, "AXRole").ok().flatten()
}

/// The app element at screen point `(x, y)`, using the target app's own
/// position so the answer is scoped to the app rather than to whatever else
/// happens to be drawn there.
fn element_at(
    app: &AXUIElement,
    x: i32,
    y: i32,
) -> Result<Option<CFRetained<AXUIElement>>, WalkError> {
    let mut slot: *const AXUIElement = std::ptr::null();
    // SAFETY: slot is a stack out-parameter. On success it holds a +1 AXUIElement.
    let error = unsafe {
        app.copy_element_at_position(
            x as core::ffi::c_float,
            y as core::ffi::c_float,
            std::ptr::NonNull::new(&mut slot).expect("slot"),
        )
    };
    match ax::classify(error) {
        ax::CopyClass::Ok => Ok(std::ptr::NonNull::new(slot.cast_mut())
            // SAFETY: Success returned a +1 element. from_raw takes that ownership.
            .map(|ptr| unsafe { CFRetained::<AXUIElement>::from_raw(ptr) })),
        ax::CopyClass::Missing => Ok(None),
        ax::CopyClass::Permission => Err(WalkError::Permission(permission_message())),
        ax::CopyClass::Timeout => Err(WalkError::Timeout),
        ax::CopyClass::Failed => Ok(None),
    }
}

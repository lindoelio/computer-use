//! Find and drive an `AXScrollArea` scroll bar so a background scroll can be
//! verified and, when needed, performed without activating the app.
//!
//! A scroll wheel posted with `CGEventPostToPid` is not delivered to the views
//! of a non-frontmost app: the post returns without error and nothing moves.
//! Writing `AXValue` on the scroll bar does move it, needs no activation, and
//! does not move the cursor. This module owns the Accessibility half of that:
//! locating the scroll area, reading and writing the bar, and estimating the
//! visible fraction that turns wheel lines into a value delta.
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

/// The scroll bar for one axis of a scroll area.
///
/// `bar` is retained on its own, so it stays valid without holding the scroll
/// area alive; the visible fraction is measured once, while both are to hand.
pub(crate) struct ScrollBar {
    bar: CFRetained<AXUIElement>,
    /// Thumb length over track length, when both sizes could be read.
    visible_fraction: Option<f64>,
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

    /// Thumb length over track length, used to size one wheel line.
    pub(crate) fn visible_fraction(&self) -> Option<f64> {
        self.visible_fraction
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

/// The scroll bar `axis` advertises on `element`, with its visible fraction.
fn scroll_bar(element: &AXUIElement, axis: crate::scroll::Axis) -> Option<ScrollBar> {
    if role(element).as_deref() != Some("AXScrollArea") {
        return None;
    }
    let bar = ax::copy_cf(element, axis.bar_attribute())
        .ok()
        .flatten()
        .and_then(|value| value.downcast::<AXUIElement>().ok())?;
    let visible_fraction = visible_fraction(element, &bar, axis);
    Some(ScrollBar {
        bar,
        visible_fraction,
    })
}

/// Scroll bar length along its axis over the scroll area's.
///
/// This is a weak proxy for the visible fraction of the content, not a
/// measurement of it. On a live `AXScrollArea` the scroll bar element spans the
/// whole track, so the ratio sits near 1.0 and one wheel line ends up moving
/// about a third of the content. AX does not say how much of a document is on
/// screen, so the caller treats the step as an approximation. See
/// [`crate::scroll::step_per_line`].
fn visible_fraction(
    area: &AXUIElement,
    bar: &AXUIElement,
    axis: crate::scroll::Axis,
) -> Option<f64> {
    let bar_length = size_along(bar, axis)?;
    let track_length = size_along(area, axis)?;
    if bar_length <= 0.0 || track_length <= 0.0 {
        return None;
    }
    let fraction = bar_length / track_length;
    (fraction.is_finite() && fraction > 0.0).then_some(fraction)
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

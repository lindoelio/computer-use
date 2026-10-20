//! Dock listing and clicking via the AX facade of `com.apple.dock`.
//!
//! The Dock is a regular AX app. We list its `AXList` items, take `AXTitle`
//! from each, and `AXPress` the one that matches the requested title.

#![allow(unsafe_code)]

use objc2_application_services::AXUIElement;
use objc2_core_foundation::{CFArray, CFBoolean, CFRetained, CFString};

use super::ax::{self, application, array_elements, classify, copy_cf, copy_string, CopyClass};

/// AXRole string for the Dock's items container. Constant so the matcher
/// and the test both reference the same value.
const AX_ROLE_LIST: &str = "AXList";
use crate::backend::{DockClickReport, DockItem, DockListReport};
use crate::error::ComputerError;

const DOCK_BUNDLE_ID: &str = "com.apple.dock";

/// List the Dock items as the Dock itself exposes them. The Dock is the
/// `com.apple.dock` process; we resolve it through `NSRunningApplication`.
pub(crate) fn list() -> Result<DockListReport, ComputerError> {
    ensure_trusted()?;
    let dock = dock_application_element()?;
    let children = copy_dock_items(&dock)?;
    let mut items = Vec::with_capacity(children.len());
    for child in children {
        let title = copy_string(&child, "AXTitle")
            .ok()
            .flatten()
            .unwrap_or_default();
        if title.is_empty() {
            continue;
        }
        items.push(DockItem { title });
    }
    Ok(DockListReport { items })
}

/// Walk the Dock's AX tree to its `AXList` of `AXDockItem` children.
///
/// The Dock's `com.apple.dock` process exposes a single AXList whose
/// children are the visible Dock items. The previous implementation read
/// the AX application element's direct AXChildren and skipped everything
/// that did not look like an app — that returned zero items even when the
/// Dock had apps, because the app element's children are unrelated system
/// nodes, not the Dock items. Walk the first AXList we hit and read its
/// AXChildren as the items.
fn copy_dock_items(dock: &AXUIElement) -> Result<Vec<CFRetained<AXUIElement>>, ComputerError> {
    let list = first_list_child(dock)?;
    copy_children(&list)
}

/// Find the Dock's `AXList` child. The Dock exposes one list per screen;
/// picking the first one matches what the user sees on the primary display.
fn first_list_child(dock: &AXUIElement) -> Result<CFRetained<AXUIElement>, ComputerError> {
    let children = copy_children(dock)?;
    let roles: Vec<String> = children
        .iter()
        .map(|child| {
            copy_string(child, "AXRole")
                .ok()
                .flatten()
                .unwrap_or_default()
        })
        .collect();
    let index = first_list_index(&roles)
        .ok_or_else(|| ComputerError::ExecutionFailed("Dock has no AXList child".into()))?;
    Ok(children.into_iter().nth(index).expect("index present"))
}

/// Pure helper: return the index of the first AXList role in `roles`.
/// Returns `None` when no element has that role. The Dock exposes one
/// list per screen; picking the first matches the primary display and
/// keeps the logic unit-testable without a live AXUIElement.
fn first_list_index<T: AsRef<str>>(roles: &[T]) -> Option<usize> {
    roles.iter().position(|role| role.as_ref() == AX_ROLE_LIST)
}

/// Press the first Dock item whose title matches `title` case-insensitively.
pub(crate) fn click(title: &str) -> Result<DockClickReport, ComputerError> {
    if title.trim().is_empty() {
        return Err(ComputerError::InvalidArguments(
            "dock click requires a non-empty title".into(),
        ));
    }
    ensure_trusted()?;
    let dock = dock_application_element()?;
    let children = copy_dock_items(&dock)?;
    let needle = title.to_lowercase();
    for child in children {
        let candidate = copy_string(&child, "AXTitle")
            .ok()
            .flatten()
            .unwrap_or_default();
        if candidate.to_lowercase() != needle {
            continue;
        }
        if !is_enabled(&child) {
            return Err(ComputerError::ExecutionFailed(format!(
                "dock item {candidate:?} is disabled"
            )));
        }
        press(&child)?;
        return Ok(DockClickReport { pressed: true });
    }
    Err(ComputerError::ElementNotFound(format!(
        "dock item {title:?} not found"
    )))
}

fn ensure_trusted() -> Result<(), ComputerError> {
    ax::ensure_trusted_for_menu()
}

fn dock_application_element() -> Result<CFRetained<AXUIElement>, ComputerError> {
    let apps = super::apps::list_apps(true)?;
    let dock = apps
        .into_iter()
        .find(|app| {
            app.bundle_id
                .as_deref()
                .map(|id| id == DOCK_BUNDLE_ID)
                .unwrap_or(false)
        })
        .ok_or_else(|| ComputerError::ExecutionFailed("com.apple.dock is not running".into()))?;
    application(dock.pid).map_err(|error| {
        ComputerError::ExecutionFailed(format!(
            "could not wrap dock AXUIElement for {}: {error:?}",
            dock.pid
        ))
    })
}

fn copy_children(element: &AXUIElement) -> Result<Vec<CFRetained<AXUIElement>>, ComputerError> {
    let value = copy_cf(element, "AXChildren").map_err(map_attr_error)?;
    let Some(cf) = value else {
        return Ok(Vec::new());
    };
    let array = match cf.downcast::<CFArray>() {
        Ok(array) => array,
        Err(_) => return Ok(Vec::new()),
    };
    Ok(array_elements(&array))
}

fn map_attr_error(error: ax::AttrError) -> ComputerError {
    match error {
        ax::AttrError::Permission => {
            ComputerError::PermissionRequired(ax::permission_message_for("Accessibility"))
        }
        ax::AttrError::Timeout => {
            ComputerError::ExecutionFailed("Accessibility call timed out".into())
        }
        ax::AttrError::Missing | ax::AttrError::Failed => {
            ComputerError::ExecutionFailed("Accessibility call failed".into())
        }
    }
}

fn is_enabled(element: &CFRetained<AXUIElement>) -> bool {
    match copy_cf(element, "AXEnabled") {
        Ok(Some(value)) => match value.downcast::<CFBoolean>() {
            Ok(boolean) => boolean.as_bool(),
            Err(_) => true,
        },
        Ok(None) => true,
        Err(_) => true,
    }
}

fn press(element: &CFRetained<AXUIElement>) -> Result<(), ComputerError> {
    let action = CFString::from_str("AXPress");
    // SAFETY: element and action are live for this call.
    let error = unsafe { element.perform_action(&action) };
    match classify(error) {
        CopyClass::Ok => Ok(()),
        CopyClass::Permission => Err(ComputerError::PermissionRequired(
            ax::permission_message_for("Accessibility"),
        )),
        CopyClass::Timeout => Err(ComputerError::ExecutionFailed(
            "AXPress on dock item timed out".into(),
        )),
        CopyClass::Missing | CopyClass::Failed => Err(ComputerError::ExecutionFailed(
            "AXPress on dock item failed".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{first_list_index, AX_ROLE_LIST};

    #[test]
    fn first_list_index_finds_the_ax_list_child() {
        // Real macOS Dock exposes a single AXList inside its app element;
        // the previous code returned ok with zero items because it read
        // AXChildren directly off the application element. This is the
        // shape that walk has to accept.
        let roles = ["AXWindow", "AXGroup", "AXList", "AXDockItem"];
        assert_eq!(first_list_index(&roles), Some(2));
    }

    #[test]
    fn first_list_index_takes_the_first_list_when_more_than_one() {
        // The Dock exposes one list per screen. Picking the first matches
        // the primary display; if a later phase wants the per-screen list,
        // that's a separate helper.
        let roles = ["AXList", "AXList", "AXDockItem"];
        assert_eq!(first_list_index(&roles), Some(0));
    }

    #[test]
    fn first_list_index_returns_none_when_no_list() {
        let roles = ["AXWindow", "AXGroup", "AXDockItem"];
        assert_eq!(first_list_index(&roles), None);
    }

    #[test]
    fn first_list_index_handles_empty_children() {
        let empty: [&str; 0] = [];
        assert_eq!(first_list_index(&empty), None);
        // The constant is what the live helper passes into first_list_index.
        assert_eq!(AX_ROLE_LIST, "AXList");
    }
}

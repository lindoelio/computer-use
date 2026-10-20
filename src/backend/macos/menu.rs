//! macOS menu bar walking: `AXMenuBar` -> `AXMenuBarItem` -> `AXMenu`.
//!
//! The list operation produces a nested tree of [`MenuNode`] up to the
//! requested depth. The click operation walks the same chain case-insensitively
//! against `AXTitle` and presses the trailing leaf with `AXPress`.

#![allow(unsafe_code)]

use objc2_application_services::AXUIElement;
use objc2_core_foundation::{CFArray, CFBoolean, CFRetained, CFString};

use super::ax::{self, application, array_elements, classify, copy_cf, copy_string, CopyClass};
use crate::backend::{MenuClickReport, MenuListReport, MenuNode};
use crate::error::ComputerError;
use crate::menu_path::MenuPath;
use crate::protocol::MENU_MAX_DEPTH;

/// Walk the menu bar and emit a nested report. `depth` caps recursion.
pub(crate) fn list(app: &str, depth: u32) -> Result<MenuListReport, ComputerError> {
    let resolved = super::apps::resolve_running(app, None)?;
    ensure_trusted()?;
    let app_element = application_for(resolved.0.pid)?;
    let bar = copy_menu_bar(&app_element)?
        .ok_or_else(|| not_found("menu bar not exposed for this app"))?;
    let depth = depth.clamp(1, MENU_MAX_DEPTH);
    let sections = collect_top_level(&bar, depth)?;
    Ok(MenuListReport {
        app: resolved.0.name,
        sections,
    })
}

/// Click a menu item by title path.
pub(crate) fn click(app: &str, path: &MenuPath) -> Result<MenuClickReport, ComputerError> {
    let resolved = super::apps::resolve_running(app, None)?;
    ensure_trusted()?;
    let app_element = application_for(resolved.0.pid)?;
    let bar = copy_menu_bar(&app_element)?
        .ok_or_else(|| not_found("menu bar not exposed for this app"))?;
    let segments = path.as_slice();
    let first = segments
        .first()
        .ok_or_else(|| not_found("menu path is empty"))?;
    let top = find_menu_bar_item(&bar, first)
        .ok_or_else(|| not_found(&format!("menu {first:?} not found")))?;
    let top_menu = menu_for_menu_bar_item(&top).ok_or_else(|| not_found("top menu not exposed"))?;
    let mut current = top_menu;
    for (index, segment) in segments.iter().enumerate() {
        let item = find_menu_item(&current, segment).ok_or_else(|| {
            not_found(&format!(
                "menu segment {} ({segment:?}) not found",
                index + 1
            ))
        })?;
        if index + 1 == segments.len() {
            if !is_enabled_item(&item) {
                return Err(ComputerError::ExecutionFailed(format!(
                    "menu segment {segment:?} is disabled"
                )));
            }
            press(&item)?;
            return Ok(MenuClickReport { pressed: true });
        }
        let Some(next_menu) = menu_for_menu_item(&item) else {
            return Err(not_found(&format!(
                "menu segment {segment:?} has no submenu"
            )));
        };
        current = next_menu;
    }
    Err(not_found("menu path resolved to no item"))
}

fn not_found(message: &str) -> ComputerError {
    ComputerError::ElementNotFound(message.to_string())
}

fn ensure_trusted() -> Result<(), ComputerError> {
    ax::ensure_trusted_for_menu()
}

fn application_for(pid: i32) -> Result<CFRetained<AXUIElement>, ComputerError> {
    application(pid).map_err(|_| {
        ComputerError::ExecutionFailed(format!("could not wrap app AXUIElement for {pid}"))
    })
}

fn copy_menu_bar(app: &AXUIElement) -> Result<Option<CFRetained<AXUIElement>>, ComputerError> {
    let value = copy_cf(app, "AXMenuBar").map_err(map_attr_error)?;
    match value {
        None => Ok(None),
        Some(cf) => match cf.downcast::<AXUIElement>() {
            Ok(element) => Ok(Some(element)),
            Err(_) => Ok(None),
        },
    }
}

fn collect_top_level(bar: &AXUIElement, depth: u32) -> Result<Vec<MenuNode>, ComputerError> {
    let items = copy_children(bar)?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        out.push(menu_bar_item_to_node(&item, depth));
    }
    Ok(out)
}

fn menu_bar_item_to_node(item: &AXUIElement, depth: u32) -> MenuNode {
    let title = copy_string(item, "AXTitle")
        .ok()
        .flatten()
        .unwrap_or_default();
    let enabled = copy_bool_attr(item, true);
    let children = if depth > 1 {
        match menu_for_menu_bar_item(item) {
            Some(menu) => collect_menu_children(&menu, depth - 1).unwrap_or_default(),
            None => Vec::new(),
        }
    } else {
        Vec::new()
    };
    MenuNode {
        title,
        enabled,
        children,
    }
}

fn is_enabled_item(item: &AXUIElement) -> bool {
    copy_bool_attr(item, false)
}

fn collect_menu_children(menu: &AXUIElement, depth: u32) -> Result<Vec<MenuNode>, ComputerError> {
    let items = copy_children(menu)?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        out.push(menu_item_to_node(&item, depth));
    }
    Ok(out)
}

fn menu_item_to_node(item: &AXUIElement, depth: u32) -> MenuNode {
    let title = copy_string(item, "AXTitle")
        .ok()
        .flatten()
        .unwrap_or_default();
    let enabled = copy_bool_attr(item, true);
    let children = if depth > 1 {
        match menu_for_menu_item(item) {
            Some(submenu) => collect_menu_children(&submenu, depth - 1).unwrap_or_default(),
            None => Vec::new(),
        }
    } else {
        Vec::new()
    };
    MenuNode {
        title,
        enabled,
        children,
    }
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

fn copy_bool_attr(element: &AXUIElement, fallback: bool) -> bool {
    match copy_cf(element, "AXEnabled") {
        Ok(Some(value)) => match value.downcast::<CFBoolean>() {
            Ok(boolean) => boolean.as_bool(),
            Err(_) => fallback,
        },
        Ok(None) => fallback,
        Err(_) => fallback,
    }
}

fn find_menu_bar_item(bar: &AXUIElement, title: &str) -> Option<CFRetained<AXUIElement>> {
    let items = copy_children(bar).ok()?;
    let needle = title.to_lowercase();
    for item in items {
        let candidate = copy_string(&item, "AXTitle")
            .ok()
            .flatten()
            .unwrap_or_default();
        if candidate.to_lowercase() == needle {
            return Some(item);
        }
    }
    None
}

fn find_menu_item(menu: &AXUIElement, title: &str) -> Option<CFRetained<AXUIElement>> {
    let items = copy_children(menu).ok()?;
    let needle = title.to_lowercase();
    for item in items {
        let candidate = copy_string(&item, "AXTitle")
            .ok()
            .flatten()
            .unwrap_or_default();
        if candidate.to_lowercase() == needle {
            return Some(item);
        }
    }
    None
}

/// Find the menu of a menu-bar item by walking its `AXChildren` for a node
/// whose role is `AXMenu`. Apple's accessibility model does not publish an
/// `AXMenu` attribute on `AXMenuBarItem` (or on a submenu `AXMenuItem`); the
/// menu is the first child whose role is `AXMenu`.
fn menu_for_menu_bar_item(item: &AXUIElement) -> Option<CFRetained<AXUIElement>> {
    first_child_with_role(item, "AXMenu")
}

/// Find the submenu of a menu item the same way: walk `AXChildren` and pick the
/// first node whose role is `AXMenu`.
fn menu_for_menu_item(item: &AXUIElement) -> Option<CFRetained<AXUIElement>> {
    first_child_with_role(item, "AXMenu")
}

/// Walk `AXChildren` and return the first node whose `AXRole` matches `role`.
///
/// Returns `None` when the children cannot be read or no child has that role.
/// This is the macOS accessibility reality for menus: a menu-bar item's menu is
/// reachable through children, not through an `AXMenu` attribute.
fn first_child_with_role(item: &AXUIElement, role: &str) -> Option<CFRetained<AXUIElement>> {
    let children = copy_children(item).ok()?;
    let roles: Vec<String> = children
        .iter()
        .map(|child| {
            copy_string(child, "AXRole")
                .ok()
                .flatten()
                .unwrap_or_default()
        })
        .collect();
    let index = first_index_with_role(&roles, role)?;
    children.into_iter().nth(index)
}

/// Pure helper: return the index of the first element whose `AXRole` matches
/// `needle`. Returns `None` when no element matches. Used by the menu walker
/// to pick the `AXMenu` child of a `AXMenuBarItem` or a submenu `AXMenuItem`.
/// Kept as a separate function so the matching logic has a unit test that
/// does not need a live AXUIElement.
fn first_index_with_role<T: AsRef<str>>(roles: &[T], needle: &str) -> Option<usize> {
    roles.iter().position(|role| role.as_ref() == needle)
}

fn press(element: &CFRetained<AXUIElement>) -> Result<(), ComputerError> {
    let action = CFString::from_str("AXPress");
    // SAFETY: element is live for this call. action is owned by us and outlives it.
    let error = unsafe { element.perform_action(&action) };
    map_action_error(error)
}

fn map_action_error(error: objc2_application_services::AXError) -> Result<(), ComputerError> {
    match classify(error) {
        CopyClass::Ok => Ok(()),
        CopyClass::Permission => Err(ComputerError::PermissionRequired(
            ax::permission_message_for("Accessibility"),
        )),
        CopyClass::Timeout => Err(ComputerError::ExecutionFailed(
            "AXPress on menu item timed out".into(),
        )),
        CopyClass::Missing | CopyClass::Failed => Err(ComputerError::ExecutionFailed(
            "AXPress on menu item failed".into(),
        )),
    }
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

#[cfg(test)]
mod tests {
    use super::first_index_with_role;

    #[test]
    fn first_index_with_role_picks_the_ax_menu_child() {
        // Real macOS Finder menu-bar items have an AXMenuBarItem whose
        // AXChildren list a single AXMenu node before any AXMenuItem.
        let roles = ["AXMenuBarItem", "AXMenu", "AXMenuItem"];
        assert_eq!(first_index_with_role(&roles, "AXMenu"), Some(1));
    }

    #[test]
    fn first_index_with_role_returns_none_when_no_menu_child() {
        // A submenu item without children returns None so the caller can
        // surface "menu segment X has no submenu".
        let roles = ["AXMenuItem", "AXMenuItem"];
        assert_eq!(first_index_with_role(&roles, "AXMenu"), None);
    }

    #[test]
    fn first_index_with_role_takes_the_first_match() {
        // The spec says the menu is the FIRST child whose role is AXMenu.
        // Anything later is a leaf the walker should ignore.
        let roles = ["AXMenuBarItem", "AXMenu", "AXMenu", "AXMenuItem"];
        assert_eq!(first_index_with_role(&roles, "AXMenu"), Some(1));
    }

    #[test]
    fn first_index_with_role_handles_empty_input() {
        let empty: [&str; 0] = [];
        assert_eq!(first_index_with_role(&empty, "AXMenu"), None);
    }
}

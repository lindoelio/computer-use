//! Actions, JSON requests, and the shared dispatcher.

use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::de::{self, Deserializer};
use serde::ser::{Serialize, Serializer};
use serde::{Deserialize, Serialize as SerializeDerive};
use serde_json::{json, Map, Value};

use crate::attempt::{self, AttemptFlags};
use crate::backend::clipboard::ClipboardContents;
use crate::backend::create_desktop_backend;
use crate::backend::{validate_key, DesktopBackend, WindowTarget};
use crate::cache;
use crate::element::{self, ElementError, ElementOp, ElementOutcome, ElementRequest};
use crate::error::{ComputerError, ErrorBody};
use crate::image_util::{self, encode_base64};
use crate::input_session::{self, InputSession};
use crate::keys::{parse_key_combo, parse_key_sequence};
use crate::policy;
use crate::state::collect_state_for;
use crate::target::{
    filter_windows_for_pid, parse_app_selector, parse_window_selector, resolve_app, resolve_window,
    WindowSelector,
};
use crate::tier::{self, InputKind, KeyStroke, TierName};
use crate::tree::TreeQuery;
use crate::types::{
    parse_path, region_inside, Frame, ImageFormat, Modifier, MouseButton, Point, Region,
    ScrollDirection,
};
use crate::vision::{OcrBlock, OcrResult};
use crate::wait;

const MAX_WAIT_MS: u128 = 24 * 60 * 60 * 1000;

/// Default recursion depth for `menu_list`. Matches the AXMenuBar chain
/// (menu bar -> menu bar item -> menu -> submenu) plus one extra level.
pub const MENU_DEFAULT_DEPTH: u32 = 4;

/// Maximum recursion depth accepted by `menu_list`. AX nesting beyond this
/// stops with a `*` placeholder so the report never explodes.
pub const MENU_MAX_DEPTH: u32 = 8;

/// Default timeout for a `shortcuts run` invocation, in seconds.
pub const SHORTCUTS_TIMEOUT_SECS: u64 = 60;

/// Default timeout for an `osascript` invocation, in seconds.
pub const APPLE_EVENT_TIMEOUT_SECS: u64 = 30;

/// One desktop action. Serialized with an `action` tag.
///
/// ```json
/// {"action":"click","x":10,"y":20}
/// ```
#[derive(Debug, Clone, PartialEq, SerializeDerive, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    /// Capture the screen or a region.
    Screenshot {
        /// File path. A temp path is used when omitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        /// Logical region, as `x,y,width,height` or an object.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        region: Option<Region>,
        /// `png` (default) or `jpeg`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<ImageFormat>,
        /// Scale the image down so its width is at most this many pixels.
        ///
        /// Absent means 1440. `0` keeps the source width.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_width: Option<u32>,
        /// Letterbox into `WxH` pixels, for example `1440x900`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        canvas: Option<String>,
        /// Include standard base64 bytes in `data`.
        #[serde(default, skip_serializing_if = "is_false")]
        base64: bool,
        /// Stamp numbered element rectangles on the image. tree indices
        /// (`n`) on a window target; OCR indices (`on`) when a tree cannot
        /// be read. `--marks` from the CLI turns this on.
        #[serde(default, skip_serializing_if = "is_false")]
        marks: bool,
    },
    /// Move to a point and click, or press an element when `element` is set.
    Click {
        /// Logical x. Omit it when `element` is set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<i32>,
        /// Logical y. Omit it when `element` is set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<i32>,
        /// Button. Defaults to left.
        #[serde(default)]
        button: MouseButton,
        /// Click twice when true. With `element`, this is `press` count 2.
        #[serde(default, skip_serializing_if = "is_false")]
        double: bool,
        /// Repeat count. 1, 2, or 3. `double: true` means 2 when `count` is 1.
        #[serde(default = "default_press_count", skip_serializing_if = "is_one")]
        count: u8,
    },
    /// Click three times.
    #[serde(rename = "triple_click", alias = "triple-click")]
    TripleClick {
        /// Logical x. Omit it when `element` is set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<i32>,
        /// Logical y. Omit it when `element` is set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<i32>,
        /// Button. Defaults to left.
        #[serde(default)]
        button: MouseButton,
    },
    /// Click twice. Equivalent to `click` with `double: true`.
    #[serde(rename = "double_click")]
    DoubleClick {
        /// Logical x. Omit it when `element` is set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<i32>,
        /// Logical y. Omit it when `element` is set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<i32>,
        /// Button. Defaults to left.
        #[serde(default)]
        button: MouseButton,
    },
    /// Press an element. `count` is 1, 2, or 3.
    Press {
        /// Repeat count. Defaults to 1.
        #[serde(default = "default_press_count")]
        count: u8,
        /// Mouse button. Left uses `AXPress`. Right uses `AXShowMenu` when advertised.
        #[serde(default)]
        button: MouseButton,
    },
    /// Button down at the first point, dragged events through the rest, up at the last.
    Drag {
        /// At least two points. A JSON array, or a flat `x,y,x,y` string.
        #[serde(alias = "points", deserialize_with = "deserialize_path")]
        path: Vec<Point>,
        /// Mouse button. Defaults to left.
        #[serde(default)]
        button: MouseButton,
    },
    /// Button down. A later `mouse_up`, or the end of a one-shot command, releases it.
    #[serde(alias = "mouse-down")]
    MouseDown {
        /// Logical x.
        x: i32,
        /// Logical y.
        y: i32,
        /// Mouse button. Defaults to left.
        #[serde(default)]
        button: MouseButton,
    },
    /// Button up.
    #[serde(alias = "mouse-up")]
    MouseUp {
        /// Logical x.
        x: i32,
        /// Logical y.
        y: i32,
        /// Mouse button. Defaults to left.
        #[serde(default)]
        button: MouseButton,
    },
    /// Key down, sleep `ms`, key up. One combination. `ms` above 30000 is refused.
    #[serde(alias = "hold-key")]
    HoldKey {
        /// One combo such as `shift` or `ctrl+alt+Delete`.
        keys: String,
        /// Milliseconds to hold.
        ms: u64,
    },
    /// Read the clipboard. Text wins over an image. Empty is `kind: empty`.
    ClipboardGet,
    /// Write the clipboard. Confirmed. The audit stores the character count, not the text.
    #[serde(alias = "clipboard-set")]
    ClipboardSet {
        /// Text to write. Empty is `invalid_arguments`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        /// PNG path. Used when `text` is absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image: Option<String>,
    },
    /// Screenshot of a region. A window target crops that region in window-local points.
    Zoom {
        /// File path. A temp path is used when omitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        /// Logical region, as `x,y,width,height` or an object. Required.
        region: Region,
        /// `png` (default) or `jpeg`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<ImageFormat>,
        /// Scale the image down so its width is at most this many pixels.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_width: Option<u32>,
        /// Letterbox into `WxH` pixels, for example `1440x900`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        canvas: Option<String>,
        /// Include standard base64 bytes in `data`.
        #[serde(default, skip_serializing_if = "is_false")]
        base64: bool,
    },
    /// Set `AXFocused` on an element.
    Focus,
    /// Set an element's value. Refused for secure fields.
    #[serde(alias = "set-value")]
    SetValue {
        /// New value.
        value: String,
    },
    /// Set the selected character range. Refused for secure fields.
    #[serde(alias = "select-text")]
    SelectText {
        /// First selected character.
        start: i64,
        /// Character after the selection.
        end: i64,
    },
    /// Perform an allowlisted accessibility action.
    #[serde(alias = "perform-action")]
    PerformAction {
        /// Action name such as `AXShowMenu`.
        name: String,
    },
    /// Scroll an element into view.
    #[serde(
        alias = "scroll-to",
        alias = "scroll_to_visible",
        alias = "scroll-to-visible"
    )]
    ScrollTo,
    /// Move the pointer without clicking.
    #[serde(alias = "move_mouse")]
    Move {
        /// Logical x.
        x: i32,
        /// Logical y.
        y: i32,
    },
    /// Type text into the focused control.
    Type {
        /// Text to type.
        text: String,
    },
    /// Press a key combination such as `cmd+shift+4`.
    Key {
        /// Combo string.
        keys: String,
    },
    /// Scroll the wheel. Positive `dy` is down. Positive `dx` is right.
    Scroll {
        /// Optional pointer x. Must be paired with `y`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<i32>,
        /// Optional pointer y. Must be paired with `x`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<i32>,
        /// Horizontal wheel steps.
        #[serde(default, alias = "scroll_x", skip_serializing_if = "Option::is_none")]
        dx: Option<i32>,
        /// Vertical wheel steps.
        #[serde(default, alias = "scroll_y", skip_serializing_if = "Option::is_none")]
        dy: Option<i32>,
        /// Direction used with `amount`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        direction: Option<ScrollDirection>,
        /// Wheel steps used with `direction`. Defaults to 1.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        amount: Option<i32>,
    },
    /// Screen, cursor, frontmost window, and a best-effort accessibility tree.
    #[serde(alias = "get-state")]
    GetState,
    /// Sleep, then return.
    Wait {
        /// Milliseconds. Added to `seconds` when both are set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ms: Option<u64>,
        /// Seconds, including fractions. Added to `ms`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seconds: Option<f64>,
        /// Wait until the element index is in the given state.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        element: Option<u32>,
        /// Element state: exists, gone, visible, enabled. Default exists.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        element_state: Option<String>,
        /// Wait until the window tree contains this text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        /// Wait until the element value contains this text. Requires `element`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        /// Restrict `text` to this element's subtree.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        within_element: Option<u32>,
        /// Wait until the window selector is exists or gone.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window_state: Option<String>,
        /// Wait until the tree differs from the session baseline.
        #[serde(default, skip_serializing_if = "is_false")]
        tree_change: bool,
        /// Wait until an app is running.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        app_running: Option<String>,
        /// Wait until the window title contains this text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        /// Case-insensitive text/value/title comparison.
        #[serde(default, skip_serializing_if = "is_false")]
        ignore_case: bool,
        /// Total time budget. Default 10000.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
        /// Poll interval. Default 100, minimum 25.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        poll_ms: Option<u64>,
    },
    /// Assert one condition with a short timeout. Failure is `verification_failed`.
    #[serde(rename = "verify")]
    Verify {
        /// Element index to assert.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        element: Option<u32>,
        /// Element state: exists, gone, visible, enabled. Default exists.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        element_state: Option<String>,
        /// Assert the window tree contains this text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        /// Assert the element value contains this text. Requires `element`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        /// Restrict `text` to this element's subtree.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        within_element: Option<u32>,
        /// Assert the window selector is exists or gone.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window_state: Option<String>,
        /// Assert the tree changed against the session baseline.
        #[serde(default, skip_serializing_if = "is_false")]
        tree_change: bool,
        /// Assert an app is running.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        app_running: Option<String>,
        /// Assert the window title contains this text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        /// Case-insensitive comparison.
        #[serde(default, skip_serializing_if = "is_false")]
        ignore_case: bool,
        /// Timeout. Default 2000.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
        /// Poll interval. Default 100.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        poll_ms: Option<u64>,
    },
    /// List displays without capturing pixels.
    Displays,
    /// Report what this build can isolate and what it cannot.
    Isolation,
    /// List running applications. `all` includes accessory and prohibited apps.
    ListApps {
        /// Include accessory and prohibited activation policies.
        #[serde(default, rename = "all")]
        all: bool,
        /// Filter by name, bundle id, or path. `*` globs are accepted.
        #[serde(default, rename = "match", skip_serializing_if = "Option::is_none")]
        r#match: Option<String>,
    },
    /// List on-screen windows. `app` comes from the shared request context.
    ListWindows {
        /// Include system windows (menubar, Dock, notifications). Default false.
        #[serde(default, rename = "all", skip_serializing_if = "is_false")]
        all: bool,
    },
    /// Launch an application named by the shared `app` field.
    Launch,
    /// Activate an application named by the shared `app` field.
    Activate,
    /// Ask an application to quit. `force` requests a forced terminate.
    Quit {
        /// Force-quit when the platform supports it. Still requires confirmation.
        #[serde(default, skip_serializing_if = "is_false")]
        force: bool,
    },
    /// Report whether policy would allow `app`. Does not perform an action.
    #[serde(rename = "policy_check")]
    PolicyCheck,
    /// Pause mutating actions until `resume`.
    Pause,
    /// Clear the pause flag.
    Resume,
    /// Report permissions and the TCC identity.
    ///
    /// The process exits 0 when the report is produced. `permissions_ok` in the
    /// body is the permission aggregate. `prompt` asks macOS to show dialogs.
    /// `request` is an alias of `prompt`.
    Doctor {
        /// Prompt for Accessibility, Screen Recording, Post Event, and Input Monitoring.
        #[serde(default, alias = "request")]
        prompt: bool,
        /// Open the first missing System Settings pane. macOS only.
        #[serde(default)]
        open_settings: bool,
        /// Linux only: force X11 detection even when WAYLAND_DISPLAY is set.
        /// Other platforms ignore the field so the dispatcher can pass it
        /// unconditionally.
        #[serde(default)]
        x11: bool,
        /// Also validate notarization of the containing app bundle. macOS only.
        #[serde(default)]
        full: bool,
    },
    /// Numbered accessibility tree for the key window.
    Tree,
    /// Restore Electron accessibility flags and stop observers.
    SessionEnd,
    /// Read the pointer. The Anthropic adapter maps `cursor_position` here.
    CursorPosition,
    /// Run several actions that share one target and one cache revision sequence.
    Batch {
        /// Stop after the first failed step. Defaults to true.
        #[serde(default = "default_stop_on_error")]
        stop_on_error: bool,
        /// Native action objects. A nested `batch` is rejected at dispatch.
        actions: Vec<Value>,
    },
    /// Absolute path of the current audit log, when one is configured.
    #[serde(rename = "audit_path", alias = "audit-path")]
    AuditPath {},
    /// Newest audit entries, reading the rotated backup first.
    #[serde(rename = "audit_tail", alias = "audit-tail")]
    AuditTail {
        /// How many lines to return. Default 50, capped at 500.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lines: Option<usize>,
    },
    /// Remove empty session directories and stale ones.
    #[serde(alias = "session-prune")]
    SessionPrune {
        /// Report matches without deleting them.
        #[serde(default)]
        dry_run: bool,
        /// Age in seconds after which a dead owner's session is stale. Absent means 24 hours.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ttl_secs: Option<u64>,
    },
    /// Walk an application's menu bar. Reads AXMenuBar -> AXMenuBarItem -> AXMenu.
    ///
    /// `--app` selects the running app. The default depth is 4; `depth` caps it.
    /// Matches are case-insensitive. Disabled items are still reported.
    #[serde(alias = "menu_list", alias = "menu-list")]
    MenuList {
        /// Override the shared `--app` flag with one of these variants: name, bundle id, or pid.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        app: Option<String>,
        /// Maximum depth below the menu bar. Default: 4. Caps recursive submenus.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        depth: Option<u32>,
    },
    /// Click a menu item by `">"`-separated titles.
    ///
    /// `--app` selects the running app. `--path` is a list of titles such as
    /// `"File > Save As > Save"`. Matches are case-insensitive. Disabled items
    /// are not pressed.
    #[serde(alias = "menu_click", alias = "menu-click")]
    MenuClick {
        /// Override the shared `--app` flag.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        app: Option<String>,
        /// Path of menu titles separated by `>`.
        path: String,
    },
    /// Send an Apple event via `osascript`. macOS only.
    ///
    /// `--app` is recorded and used by the deny policy; the script body decides
    /// which application the event targets. `--language` is `applescript` (the
    /// default) or `javascript`. The script body is never logged verbatim.
    #[serde(alias = "apple_event", alias = "apple-event")]
    AppleEvent {
        /// Override the shared `--app` flag.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        app: Option<String>,
        /// Script body passed to `osascript -e`. Never logged verbatim.
        script: String,
        /// `applescript` (default) or `javascript`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<AppleEventLanguage>,
    },
    /// Run a Shortcuts workflow by name. macOS only.
    ///
    /// The `shortcuts` binary is called with the workflow name as a single
    /// argument. A missing binary is `execution_failed`.
    #[serde(alias = "shortcuts_run", alias = "shortcuts-run")]
    ShortcutsRun {
        /// Workflow name.
        name: String,
    },
    /// List the Dock items. macOS only.
    #[serde(alias = "dock_list", alias = "dock-list")]
    DockList {},
    /// Click a Dock item by title. macOS only.
    #[serde(alias = "dock_click", alias = "dock-click")]
    DockClick {
        /// Dock item title.
        title: String,
    },
    /// Group the on-screen windows by the screen that contains each window's center.
    ///
    /// Mission Control moves are not attempted; the report only describes the
    /// current layout. The skylight feature, when active, may move windows
    /// between spaces; otherwise the action reports the visible state only.
    #[serde(alias = "spaces")]
    Spaces {},
    /// Move the front (or `--window`-selected) window to another space.
    ///
    /// Requires the private SkyLight window-to-space API, which this build
    /// does not implement; the action always returns `unsupported` with the
    /// reason. `window move --to-display` is the supported alternative.
    #[serde(rename = "spaces_move", alias = "spaces-move")]
    SpacesMove {
        /// Managed space index, 1-based, as reported by `spaces`.
        to_space: u32,
    },
    /// List the tabs the running Chromium exposes. Requires `--features cdp`.
    /// Without the feature this returns `unsupported`.
    #[serde(rename = "browser_tabs", alias = "browser-tabs")]
    BrowserTabs {},
    /// Activate a tab by `target_id`. Requires `--features cdp`.
    #[serde(rename = "browser_activate", alias = "browser-activate")]
    BrowserActivate {
        /// CDP `targetId` to bring to the front.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
    },
    /// Navigate a tab to `url`. Requires `--features cdp`.
    #[serde(rename = "browser_navigate", alias = "browser-navigate")]
    BrowserNavigate {
        /// Destination URL.
        url: String,
        /// Restrict to one tab.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
        /// `none`, `load`, `domcontentloaded`, `networkidle`,
        /// `selector:<css>`, or `url:<glob>`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait: Option<String>,
        /// Timeout in milliseconds.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
        /// Open the URL in a new tab.
        #[serde(default, skip_serializing_if = "is_false")]
        new_tab: bool,
    },
    /// Go back in the tab's history.
    #[serde(rename = "browser_back", alias = "browser-back")]
    BrowserBack {
        /// Restrict to one tab.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
    },
    /// Go forward in the tab's history.
    #[serde(rename = "browser_forward", alias = "browser-forward")]
    BrowserForward {
        /// Restrict to one tab.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
    },
    /// Reload the tab.
    #[serde(rename = "browser_reload", alias = "browser-reload")]
    BrowserReload {
        /// Restrict to one tab.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
        /// Wait condition after the reload.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait: Option<String>,
        /// Timeout in milliseconds.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    /// Wait for a condition without navigating.
    #[serde(rename = "browser_wait", alias = "browser-wait")]
    BrowserWait {
        /// Restrict to one tab.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
        /// `none`, `load`, `domcontentloaded`, `networkidle`,
        /// `selector:<css>`, or `url:<glob>`.
        wait: String,
        /// Timeout in milliseconds.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    /// Capture a compact accessibility snapshot with stable refs.
    #[serde(rename = "browser_snapshot", alias = "browser-snapshot")]
    BrowserSnapshot {
        /// Restrict to one tab.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
        /// Return only the delta against the cached snapshot.
        #[serde(default, skip_serializing_if = "is_false")]
        diff: bool,
        /// Only actionable controls.
        #[serde(default, skip_serializing_if = "is_false")]
        interactive_only: bool,
        /// Maximum rendered nodes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_nodes: Option<usize>,
    },
    /// Click an element by CSS selector or snapshot ref.
    #[serde(rename = "browser_click", alias = "browser-click")]
    BrowserClick {
        /// CSS selector. Mutually exclusive with `ref_id`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        /// Snapshot ref such as `e12`. Mutually exclusive with `selector`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ref_id: Option<String>,
        /// Restrict to one tab.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
        /// Mouse button.
        #[serde(default)]
        button: MouseButton,
        /// Click count: 1, 2, or 3.
        #[serde(default = "default_press_count")]
        count: u8,
        /// Wait condition after the click.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait: Option<String>,
        /// Timeout in milliseconds.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
        /// Skip visibility/enabled checks.
        #[serde(default, skip_serializing_if = "is_false")]
        force: bool,
    },
    /// Read the `innerText` of an element by CSS selector or snapshot ref.
    #[serde(rename = "browser_text", alias = "browser-text")]
    BrowserText {
        /// CSS selector. Mutually exclusive with `ref_id`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        /// Snapshot ref such as `e12`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ref_id: Option<String>,
        /// Restrict to one tab.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
    },
    /// Type `text` into an element. Refuses secure fields.
    #[serde(rename = "browser_type", alias = "browser-type")]
    BrowserType {
        /// Text to type.
        text: String,
        /// CSS selector. Mutually exclusive with `ref_id`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        /// Snapshot ref such as `e12`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ref_id: Option<String>,
        /// Restrict to one tab.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
        /// Clear the field before typing.
        #[serde(default, skip_serializing_if = "is_false")]
        clear: bool,
        /// Press Enter after typing.
        #[serde(default, skip_serializing_if = "is_false")]
        submit: bool,
        /// Wait condition after typing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait: Option<String>,
        /// Timeout in milliseconds.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    /// Press a key combination.
    #[serde(rename = "browser_press", alias = "browser-press")]
    BrowserPress {
        /// Combination such as `Enter`, `cmd+a`, or `ctrl+shift+Tab`.
        keys: String,
        /// Optional element to focus first.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        /// Optional snapshot ref to focus first.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ref_id: Option<String>,
        /// Restrict to one tab.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
    },
    /// Move the mouse over an element.
    #[serde(rename = "browser_hover", alias = "browser-hover")]
    BrowserHover {
        /// CSS selector. Mutually exclusive with `ref_id`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        /// Snapshot ref.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ref_id: Option<String>,
        /// Restrict to one tab.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
    },
    /// Scroll the page or an element.
    #[serde(rename = "browser_scroll", alias = "browser-scroll")]
    BrowserScroll {
        /// CSS selector. Mutually exclusive with `ref_id`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        /// Snapshot ref.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ref_id: Option<String>,
        /// Restrict to one tab.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
        /// Horizontal wheel delta.
        #[serde(default, skip_serializing_if = "is_zero_f64")]
        dx: f64,
        /// Vertical wheel delta. Positive is down.
        #[serde(default, skip_serializing_if = "is_zero_f64")]
        dy: f64,
    },
    /// Select an option in a `<select>`.
    #[serde(rename = "browser_select", alias = "browser-select")]
    BrowserSelect {
        /// CSS selector. Mutually exclusive with `ref_id`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        /// Snapshot ref.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ref_id: Option<String>,
        /// Restrict to one tab.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
        /// Option value.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        /// Option label.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Check or uncheck a checkbox or radio.
    #[serde(rename = "browser_check", alias = "browser-check")]
    BrowserCheck {
        /// CSS selector. Mutually exclusive with `ref_id`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        /// Snapshot ref.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ref_id: Option<String>,
        /// Restrict to one tab.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
        /// Desired state. Default true.
        #[serde(default = "default_true")]
        checked: bool,
    },
    /// Set files on an `<input type=file>`.
    #[serde(rename = "browser_upload", alias = "browser-upload")]
    BrowserUpload {
        /// CSS selector. Mutually exclusive with `ref_id`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        /// Snapshot ref.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ref_id: Option<String>,
        /// Restrict to one tab.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
        /// Absolute file paths.
        files: Vec<String>,
    },
    /// Accept or dismiss a JavaScript dialog.
    #[serde(rename = "browser_dialog", alias = "browser-dialog")]
    BrowserDialog {
        /// Restrict to one tab.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
        /// Accept when true, dismiss when false.
        #[serde(default = "default_true")]
        accept: bool,
        /// Text for a prompt dialog.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt_text: Option<String>,
    },
    /// Capture a page screenshot.
    #[serde(rename = "browser_screenshot", alias = "browser-screenshot")]
    BrowserScreenshot {
        /// Restrict to one tab.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
        /// Output path. Defaults to a temp file.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        /// Capture beyond the viewport.
        #[serde(default, skip_serializing_if = "is_false")]
        full_page: bool,
        /// `png` or `jpeg`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        format: Option<String>,
        /// JPEG quality.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quality: Option<i64>,
        /// Include the image as base64 in the response.
        #[serde(default, skip_serializing_if = "is_false")]
        base64: bool,
    },
    /// Evaluate `script` in the page context and return the JSON value.
    #[serde(rename = "browser_eval", alias = "browser-eval")]
    BrowserEval {
        /// JavaScript source.
        script: String,
        /// Restrict to one tab.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_id: Option<String>,
        /// Timeout in milliseconds.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
        /// Await a returned promise.
        #[serde(default, skip_serializing_if = "is_false")]
        await_promise: bool,
    },
    /// Open a new tab.
    #[serde(rename = "browser_tab_new", alias = "browser-tab-new")]
    BrowserTabNew {
        /// URL to open. Defaults to `about:blank`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    /// Close a tab.
    #[serde(rename = "browser_tab_close", alias = "browser-tab-close")]
    BrowserTabClose {
        /// CDP `targetId`.
        target_id: String,
    },
    /// Start a managed Chromium with an isolated profile.
    #[serde(rename = "browser_launch", alias = "browser-launch")]
    BrowserLaunch {
        /// Browser executable name or path.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        browser: Option<String>,
        /// Run headless.
        #[serde(default, skip_serializing_if = "is_false")]
        headless: bool,
        /// Profile directory. Defaults to a session directory in the cache.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<String>,
        /// Debug port. Default 0 (an ephemeral port).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        port: Option<u16>,
        /// URL to open. Defaults to `about:blank`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    /// Stop a managed Chromium.
    #[serde(rename = "browser_quit", alias = "browser-quit")]
    BrowserQuit {
        /// Managed session id. Defaults to the newest one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        /// Remove the profile directory too.
        #[serde(default, skip_serializing_if = "is_false")]
        purge: bool,
    },
    /// List managed Chromium sessions.
    #[serde(rename = "browser_sessions", alias = "browser-sessions")]
    BrowserSessions {},
    /// Run several browser actions in one call. One process, one connection
    /// per step, and one result array. A nested batch is `invalid_arguments`.
    #[serde(rename = "browser_batch", alias = "browser-batch")]
    BrowserBatch {
        /// Browser actions. Each entry uses the native `action` tag.
        actions: Vec<Action>,
        /// Stop at the first failure. Default true.
        #[serde(default = "default_true", skip_serializing_if = "is_true")]
        stop_on_error: bool,
    },
    /// Run OCR on the most recent capture for `--app`/window target. macOS
    /// only. Linux returns `unsupported`; Windows is a stub until P12.
    #[serde(rename = "ocr")]
    Ocr {
        /// Re-run OCR even when `--app` already produced it. Defaults to false.
        #[serde(default, skip_serializing_if = "is_false")]
        force: bool,
        /// Run OCR on a file instead of a live capture. `screen` then equals
        /// the image pixel. No secure-field redaction applies.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image: Option<String>,
    },
    /// Move one window. Requires `x` and `y`, or `to_display`.
    #[serde(rename = "window_move", alias = "window-move")]
    WindowMove {
        /// Left edge in screen points.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<i32>,
        /// Top edge in screen points.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<i32>,
        /// Center the window on this display instead of `x`/`y`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to_display: Option<u32>,
    },
    /// Resize one window.
    #[serde(rename = "window_resize", alias = "window-resize")]
    WindowResize {
        /// New width in screen points.
        width: u32,
        /// New height in screen points.
        height: u32,
    },
    /// Set any combination of position and size on one window.
    #[serde(rename = "window_set", alias = "window-set")]
    WindowSet {
        /// Left edge in screen points.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<i32>,
        /// Top edge in screen points.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<i32>,
        /// New width in screen points.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<u32>,
        /// New height in screen points.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
    },
    /// Read or set `AXMinimized`/`AXFullScreen` on one window.
    #[serde(rename = "window_state", alias = "window-state")]
    WindowState {
        /// Set the minimized flag.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        minimized: Option<bool>,
        /// Set the full-screen flag.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fullscreen: Option<bool>,
    },
    /// Report one window's bounds, display, and status.
    #[serde(rename = "window_info", alias = "window-info")]
    WindowInfo,
}

fn default_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_zero_f64(value: &f64) -> bool {
    *value == 0.0
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_one(value: &u8) -> bool {
    *value == 1
}

fn default_press_count() -> u8 {
    1
}

/// Language of an Apple event script. macOS only.
///
/// `applescript` is the default. `javascript` selects the JavaScript for
/// Automation (`JXA`) runtime in `osascript`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, SerializeDerive, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppleEventLanguage {
    /// AppleScript. Default.
    #[default]
    #[serde(alias = "applescript", alias = "osa")]
    AppleScript,
    /// JavaScript for Automation. `osascript -l JavaScript`.
    #[serde(alias = "javascript", alias = "js", alias = "JavaScript")]
    JavaScript,
}

impl AppleEventLanguage {
    /// Short tag passed to `osascript -l`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AppleScript => "AppleScript",
            Self::JavaScript => "JavaScript",
        }
    }
}

fn deserialize_path<'de, D>(deserializer: D) -> Result<Vec<Point>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::String(text) => parse_path(&text).map_err(de::Error::custom),
        Value::Array(_) => serde_json::from_value(value).map_err(de::Error::custom),
        _ => Err(de::Error::custom(
            "path must be an array of points or an x,y,x,y string",
        )),
    }
}

fn default_stop_on_error() -> bool {
    true
}

/// Flags shared by every action. JSON keys are removed before the action is decoded.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CallContext {
    /// Target application: name, bundle id, or pid.
    pub app: Option<String>,
    /// Target window: title, z-order index, or `id:<number>`.
    pub window: Option<String>,
    /// Element index in the current revision. Used from P3.
    pub element: Option<u32>,
    /// Emit a tree diff when a baseline exists.
    pub diff: bool,
    /// Replace the tree baseline with the full tree.
    pub full: bool,
    /// Attach post-action state. Used from P5.
    pub return_state: bool,
    /// Attach only the post-action delta instead of the full tree.
    pub state_diff: bool,
    /// Attach a screenshot with post-action state. Used from P5.
    pub include_screenshot: bool,
    /// Coordinate space name. Used from P5.
    pub coord: Option<String>,
    /// Cache namespace. Defaults to the process id in later phases.
    pub session: Option<String>,
    /// Input tier name. Used from P7.
    pub tier: Option<String>,
    /// Adapter name. Used from P5.
    pub adapter: Option<String>,
    /// Display filter. Used from P14.
    pub display: Option<String>,
    /// Fail when the element is outside the window and scroll-into-view cannot run.
    pub require_visible: bool,
    /// User policy file from `--policy`. Absent uses the default path.
    pub policy: Option<String>,
    /// Confirm hook from `--confirm-cmd`. Wins over the environment and the policy file.
    pub confirm_cmd: Option<String>,
    /// Skip `foreground.lock` for tier `activate`.
    pub allow_foreground: bool,
    /// Refuse tier `activate`, and treat cursor motion as failure.
    pub require_background: bool,
    /// Do not send the agent point to the overlay helper.
    pub no_overlay: bool,
    /// Route browser commands through CDP. The dispatcher refuses the request
    /// when this flag is off (`invalid_arguments` for the CLI, `unsupported`
    /// for the JSON path).
    pub cdp: bool,
    /// Override the discovered CDP websocket URL. Wins over `--app` argv
    /// discovery. Accepts `ws://`, `wss://`, `http://`, `https://`.
    pub cdp_url: Option<String>,
    /// Cached `[browser].blocked_urls` from the loaded policy. The dispatcher
    /// refuses the websocket connection (and the page activation) when the
    /// destination URL is in here.
    pub browser_blocked_urls: Vec<String>,
    /// What to do when a JavaScript dialog opens: `accept`, `dismiss`, `fail`.
    pub on_dialog: Option<String>,
    /// Visible agent marker: `auto`, `on`, `off`.
    pub agent_overlay: Option<String>,
}

/// A JSON request: an optional `id`, shared flags, and an [`Action`].
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// Caller-supplied id, echoed on the response when present.
    pub id: Option<Value>,
    /// Action to run.
    pub action: Action,
    /// Flags that were siblings of `action`.
    pub context: CallContext,
}

/// Parse one JSON object. `id` and the shared flag keys are removed before the action is decoded.
pub fn parse_request(mut value: Value) -> Result<Request, ComputerError> {
    if !value.is_object() {
        return Err(ComputerError::InvalidArguments(
            "request must be a JSON object".into(),
        ));
    }
    let object = value
        .as_object_mut()
        .expect("object check above guarantees a map");
    let id = object.remove("id");
    let context = take_context(object)?;
    let action = serde_json::from_value(value)
        .map_err(|error| ComputerError::InvalidArguments(error.to_string()))?;
    Ok(Request {
        id,
        action,
        context,
    })
}

fn take_context(object: &mut Map<String, Value>) -> Result<CallContext, ComputerError> {
    let state_diff = take_bool(object, "state_diff")?;
    // `wait` and `verify` use `element` as the condition target, not as the
    // shared context element. Leaving the key in the object lets the action
    // deserialize it; the context element stays unset.
    let element = if element_is_a_condition(object) {
        None
    } else {
        take_optional_u32(object, "element")?
    };
    Ok(CallContext {
        app: take_stringish(object, "app")?,
        window: take_stringish(object, "window")?,
        element,
        diff: take_bool(object, "diff")?,
        full: take_bool(object, "full")?,
        return_state: take_bool(object, "return_state")? || state_diff,
        state_diff,
        include_screenshot: take_bool(object, "include_screenshot")?,
        coord: take_stringish(object, "coord")?,
        session: take_stringish(object, "session")?,
        tier: take_stringish(object, "tier")?,
        adapter: take_stringish(object, "adapter")?,
        display: take_stringish(object, "display")?,
        require_visible: take_bool(object, "require_visible")?,
        policy: None,
        confirm_cmd: None,
        allow_foreground: take_bool(object, "allow_foreground")?,
        require_background: take_bool(object, "require_background")?,
        no_overlay: take_bool(object, "no_overlay")?,
        cdp: take_bool(object, "cdp")?,
        cdp_url: take_stringish(object, "cdp_url")?,
        browser_blocked_urls: take_string_list(object, "browser_blocked_urls")?,
        on_dialog: take_stringish(object, "on_dialog")?,
        agent_overlay: take_stringish(object, "agent_overlay")?,
    })
}

/// True when `element` on this request is a `wait`/`verify` condition rather
/// than the shared context element.
fn element_is_a_condition(object: &Map<String, Value>) -> bool {
    matches!(
        object.get("action").and_then(Value::as_str),
        Some("wait") | Some("verify")
    )
}

fn take_stringish(
    object: &mut Map<String, Value>,
    key: &str,
) -> Result<Option<String>, ComputerError> {
    match object.remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(Value::Number(number)) => Ok(Some(number.to_string())),
        Some(_) => Err(ComputerError::InvalidArguments(format!(
            "{key} must be a string or number"
        ))),
    }
}

fn take_bool(object: &mut Map<String, Value>, key: &str) -> Result<bool, ComputerError> {
    match object.remove(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(value),
        Some(_) => Err(ComputerError::InvalidArguments(format!(
            "{key} must be a boolean"
        ))),
    }
}

fn take_string_list(
    object: &mut Map<String, Value>,
    key: &str,
) -> Result<Vec<String>, ComputerError> {
    match object.remove(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .into_iter()
            .map(|value| match value {
                Value::String(text) => Ok(text),
                _ => Err(ComputerError::InvalidArguments(format!(
                    "{key} entries must be strings"
                ))),
            })
            .collect(),
        Some(_) => Err(ComputerError::InvalidArguments(format!(
            "{key} must be a list of strings"
        ))),
    }
}

fn take_optional_u32(
    object: &mut Map<String, Value>,
    key: &str,
) -> Result<Option<u32>, ComputerError> {
    match object.remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| ComputerError::InvalidArguments(format!("{key} is out of range"))),
        Some(Value::String(text)) => text
            .parse::<u32>()
            .map(Some)
            .map_err(|_| ComputerError::InvalidArguments(format!("{key} must be an integer"))),
        Some(_) => Err(ComputerError::InvalidArguments(format!(
            "{key} must be an integer"
        ))),
    }
}

/// Decode one JSON line and dispatch it.
///
/// Parse errors return `ok: false` and do not panic. The adapter is native
/// unless the object sets `adapter`.
pub fn handle_request_line(line: &str) -> Response {
    handle_request_line_with(line, None)
}

/// Options from the CLI that JSON requests do not carry.
#[derive(Debug, Clone, Copy, Default)]
pub struct LineOptions<'a> {
    /// Process `--adapter` flag. A request field wins.
    pub adapter: Option<&'a str>,
    /// Process `--policy` path.
    pub policy: Option<&'a str>,
    /// Process `--confirm-cmd` program.
    pub confirm_cmd: Option<&'a str>,
    /// Process `--tier`. A request field wins. `--synthetic-focus` arrives as `synthetic`.
    pub tier: Option<&'a str>,
    /// Process `--allow-foreground`. Combined with the request by or.
    pub allow_foreground: bool,
    /// Process `--require-background`. Combined with the request by or.
    pub require_background: bool,
    /// Process `--no-overlay`. Combined with the request by or.
    pub no_overlay: bool,
    /// Process `--cdp` flag. Combined with the request by or.
    pub cdp: bool,
    /// Process `--cdp-url` flag. A request field wins.
    pub cdp_url: Option<&'a str>,
    /// Process `--on-dialog` flag. A request field wins.
    pub on_dialog: Option<&'a str>,
    /// Process `--agent-overlay` flag. A request field wins.
    pub agent_overlay: Option<&'a str>,
}

/// Decode one JSON line. `cli_adapter` is the process `--adapter` flag.
///
/// A request field `adapter` wins. `native` skips translation. There is no
/// detection when both are absent.
pub fn handle_request_line_with(line: &str, cli_adapter: Option<&str>) -> Response {
    handle_request_line_opts(
        line,
        LineOptions {
            adapter: cli_adapter,
            ..LineOptions::default()
        },
    )
}

/// Decode one JSON line and apply CLI `--policy` and `--confirm-cmd`.
pub fn handle_request_line_opts(line: &str, options: LineOptions<'_>) -> Response {
    let value = match serde_json::from_str::<Value>(line) {
        Ok(value) => value,
        Err(error) => {
            return Response::failure("invalid_arguments", format!("invalid JSON: {error}"))
        }
    };
    let id = value.get("id").cloned();
    let echo = match effective_adapter(&value, options.adapter) {
        Ok(name) => name,
        Err(error) => return Response::from_error(error).with_id(id),
    };
    match build_request(value, echo.as_deref()) {
        Ok(mut request) => {
            if let Some(policy) = options.policy {
                request.context.policy = Some(policy.to_string());
            }
            if let Some(confirm) = options.confirm_cmd {
                request.context.confirm_cmd = Some(confirm.to_string());
            }
            if request.context.tier.is_none() {
                if let Some(tier) = options.tier {
                    request.context.tier = Some(tier.to_string());
                }
            }
            request.context.allow_foreground |= options.allow_foreground;
            request.context.require_background |= options.require_background;
            request.context.no_overlay |= options.no_overlay;
            request.context.cdp |= options.cdp;
            if request.context.cdp_url.is_none() {
                if let Some(url) = options.cdp_url {
                    request.context.cdp_url = Some(url.to_string());
                }
            }
            if request.context.on_dialog.is_none() {
                if let Some(mode) = options.on_dialog {
                    request.context.on_dialog = Some(mode.to_string());
                }
            }
            if request.context.agent_overlay.is_none() {
                if let Some(mode) = options.agent_overlay {
                    request.context.agent_overlay = Some(mode.to_string());
                }
            }
            dispatch_context(request.action, &request.context).with_id(request.id)
        }
        Err(error) => {
            let mut response = Response::from_error(error).with_id(id);
            stamp_adapter_name(&mut response, echo.as_deref());
            response
        }
    }
}

fn effective_adapter(
    value: &Value,
    cli_adapter: Option<&str>,
) -> Result<Option<String>, ComputerError> {
    if !value.is_object() {
        return Ok(None);
    }
    let from_request = match value.get("adapter") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.as_str()),
        Some(_) => {
            return Err(ComputerError::InvalidArguments(
                "adapter must be a string".into(),
            ))
        }
    };
    let name = from_request.or(cli_adapter);
    match name {
        None | Some("native") => Ok(None),
        Some("openai" | "anthropic") => Ok(name.map(str::to_string)),
        Some(other) => Err(ComputerError::InvalidArguments(format!(
            "unknown adapter {other:?}"
        ))),
    }
}

fn build_request(value: Value, adapter: Option<&str>) -> Result<Request, ComputerError> {
    let Some(name) = adapter else {
        return parse_request(value);
    };
    let mut value = value;
    let object = value
        .as_object_mut()
        .expect("effective_adapter only names an adapter for an object");
    let id = object.remove("id");
    let mut context = take_context(object)?;
    context.coord = Some("image".into());
    context.adapter = Some(name.to_string());
    let action = crate::adapters::translate(name, &value)?;
    Ok(Request {
        id,
        action,
        context,
    })
}

fn stamp_adapter_name(response: &mut Response, adapter: Option<&str>) {
    if let Some(name @ ("openai" | "anthropic")) = adapter {
        response.fields.insert("adapter".into(), json!(name));
    }
}

/// Run `action` on the platform backend.
pub fn dispatch(action: Action) -> Response {
    dispatch_context(action, &CallContext::default())
}

/// Run `action` with shared flags on the platform backend.
pub fn dispatch_context(action: Action, context: &CallContext) -> Response {
    let backend = create_desktop_backend();
    dispatch_with_context(backend.as_ref(), action, context)
}

/// Run `action` on `backend`. Panics inside the backend become an error response.
pub fn dispatch_with(backend: &dyn DesktopBackend, action: Action) -> Response {
    dispatch_with_context(backend, action, &CallContext::default())
}

/// Run `action` on `backend` with shared flags.
pub fn dispatch_with_context(
    backend: &dyn DesktopBackend,
    action: Action,
    context: &CallContext,
) -> Response {
    let mut response = match catch_unwind(AssertUnwindSafe(|| {
        dispatch_inner(backend, action, context)
    })) {
        Ok(response) => response,
        Err(_) => Response::failure("execution_failed", "action panicked and was recovered"),
    };
    stamp_adapter_name(&mut response, context.adapter.as_deref());
    // A serve or batch session stays open so a later mouse_up can pair with this down.
    // A one-shot command has no session, so anything still held is released here.
    if !input_session::session_open() {
        let _ = backend.release_held();
    }
    response
}

enum PostKind {
    /// Successful or failed mutating action. `return_state` snapshots afterward.
    Mutating,
    /// `return_state` and `include_screenshot` are ignored.
    ReadOnly,
    /// The action already captured. `include_screenshot` reuses that body.
    Screenshot,
}

fn dispatch_inner(backend: &dyn DesktopBackend, action: Action, context: &CallContext) -> Response {
    let label = action_label(&action);
    let chars = clipboard_chars(&action);
    let action = match remap_coordinates(context, action) {
        Ok(action) => action,
        Err(error) => {
            return finish_action(
                context,
                label,
                &policy::Subject::default(),
                None,
                "allow",
                "default",
                chars,
                Response::from_error(error),
            )
        }
    };
    if let Err(error) = preflight_action(&action) {
        return finish_action(
            context,
            label,
            &policy::Subject::default(),
            None,
            "allow",
            "default",
            chars,
            Response::from_error(error),
        );
    }
    if let Err(error) = gate_display(backend, context, &action) {
        return finish_action(
            context,
            label,
            &policy::Subject::default(),
            None,
            "allow",
            "default",
            chars,
            Response::from_error(error),
        );
    }
    let prepared = prepare_action(backend, &action, context);
    if let Some(response) = prepared.stop {
        // `get-state` without an explicit app still returns the metadata
        // (displays, cursor, frontmost window) when the frontmost app is on
        // the deny list. The tree is omitted; the agent gets orientation
        // instead of a bare policy_denied.
        if matches!(action, Action::GetState)
            && context.app.is_none()
            && response
                .error
                .as_ref()
                .is_some_and(|error| error.code == "policy_denied")
        {
            let mut value = crate::state::collect_state_metadata(backend);
            let reason = response
                .error
                .as_ref()
                .map(|error| error.message.clone())
                .unwrap_or_default();
            value["policy"] = json!({
                "decision": "deny",
                "source": prepared.source,
                "reason": reason,
            });
            let extra =
                "the frontmost app is on the deny list, so the accessibility tree is omitted";
            let note = value
                .get("note")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(|text| format!("{text}; {extra}"))
                .unwrap_or_else(|| extra.to_string());
            value["note"] = json!(note);
            return finish_action(
                context,
                label,
                &prepared.subject,
                prepared.warning.as_deref(),
                "deny",
                prepared.source,
                chars,
                Response::success(value),
            );
        }
        return finish_action(
            context,
            label,
            &prepared.subject,
            prepared.warning.as_deref(),
            prepared.decision,
            prepared.source,
            chars,
            response,
        );
    }
    let kind = post_state_kind(&action);
    let response = match action {
        Action::Batch {
            actions,
            stop_on_error,
        } => {
            return finish_action(
                context,
                label,
                &prepared.subject,
                prepared.warning.as_deref(),
                prepared.decision,
                prepared.source,
                chars,
                run_batch(backend, context, actions, stop_on_error),
            )
        }
        Action::Screenshot {
            output,
            region,
            format,
            max_width,
            canvas,
            base64,
            marks,
        } => result_response(screenshot(
            backend,
            context,
            ScreenshotArgs {
                output,
                region,
                format,
                max_width,
                canvas,
                include_base64: base64,
                window_crop: false,
                marks,
            },
        )),
        Action::Zoom {
            output,
            region,
            format,
            max_width,
            canvas,
            base64,
        } => result_response(screenshot(
            backend,
            context,
            ScreenshotArgs {
                output,
                region: Some(region),
                format,
                max_width,
                canvas,
                include_base64: base64,
                window_crop: true,
                marks: false,
            },
        )),
        Action::Click {
            x,
            y,
            button,
            double,
            count,
        } => click(backend, context, x, y, button, double, count),
        Action::DoubleClick { x, y, button } => click(backend, context, x, y, button, true, 1),
        Action::TripleClick { x, y, button } => click(backend, context, x, y, button, false, 3),
        Action::Press { count, button } => {
            element_command(backend, context, ElementOp::Press { count, button })
        }
        Action::Drag { path, button } => drag(backend, context, path, button),
        Action::MouseDown { x, y, button } => pointer_half(backend, context, x, y, button, true),
        Action::MouseUp { x, y, button } => pointer_half(backend, context, x, y, button, false),
        Action::HoldKey { keys, ms } => hold_key(backend, context, &keys, ms),
        Action::ClipboardGet => result_response(clipboard_get(backend, context)),
        Action::ClipboardSet { text, image } => {
            result_response(clipboard_set(backend, text, image))
        }
        Action::Focus => element_command(backend, context, ElementOp::Focus),
        Action::SetValue { value } => {
            element_command(backend, context, ElementOp::SetValue { value })
        }
        Action::SelectText { start, end } => {
            element_command(backend, context, ElementOp::SelectText { start, end })
        }
        Action::PerformAction { name } => {
            element_command(backend, context, ElementOp::Perform { name })
        }
        Action::ScrollTo => element_command(backend, context, ElementOp::ScrollTo),
        Action::Move { x, y } => move_mouse(backend, context, x, y),
        Action::Type { text } => type_command(backend, context, text),
        Action::Key { keys } => press_key(backend, context, &keys),
        Action::Scroll {
            x,
            y,
            dx,
            dy,
            direction,
            amount,
        } => scroll(backend, context, (x, y), (dx, dy), direction, amount),
        Action::GetState => result_response(Ok(collect_state_for(backend, &tree_query(context)))),
        Action::Tree => result_response(tree_action(backend, context)),
        Action::Ocr { force, image } => {
            result_response(ocr_action(backend, context, force, image.as_deref()))
        }
        Action::WindowMove { x, y, to_display } => {
            result_response(window_move_action(backend, context, x, y, to_display))
        }
        Action::WindowResize { width, height } => result_response(window_set_action(
            backend,
            context,
            None,
            None,
            Some(width),
            Some(height),
            "window_resize",
        )),
        Action::WindowSet {
            x,
            y,
            width,
            height,
        } => result_response(window_set_action(
            backend,
            context,
            x,
            y,
            width,
            height,
            "window_set",
        )),
        Action::WindowState {
            minimized,
            fullscreen,
        } => result_response(window_state_action(backend, context, minimized, fullscreen)),
        Action::WindowInfo => result_response(window_info_action(backend, context)),
        Action::SessionEnd => result_response(session_end(backend, context)),
        wait @ Action::Wait { .. } => result_response(wait_action(backend, context, &wait)),
        verify @ Action::Verify { .. } => verify_response(backend, context, &verify),
        Action::Displays => result_response(displays(backend)),
        Action::Isolation => result_response(isolation()),
        Action::ListApps { all, r#match } => {
            result_response(list_apps(backend, all, r#match.as_deref()))
        }
        Action::ListWindows { all } => {
            result_response(list_windows(backend, context.app.as_deref(), all))
        }
        Action::Launch => result_response(launch(backend, context.app.as_deref())),
        Action::Activate => result_response(activate(
            backend,
            context.app.as_deref(),
            context.window.as_deref(),
        )),
        Action::Quit { force } => result_response(quit(backend, context.app.as_deref(), force)),
        Action::Doctor {
            prompt,
            open_settings,
            x11,
            full,
        } => result_response(doctor(
            backend,
            crate::doctor::DoctorOptions {
                prompt,
                open_settings,
                x11,
                full,
            },
        )),
        Action::CursorPosition => result_response(cursor_position(backend)),
        Action::AuditPath {} => result_response(Ok(json!({
            "path": crate::audit::audit_path().map(|path| path.display().to_string()),
        }))),
        Action::AuditTail { lines } => result_response(
            crate::audit::tail(lines.unwrap_or(50))
                .map(|entries| json!({"lines": entries, "count": entries.len()})),
        ),
        Action::SessionPrune { dry_run, ttl_secs } => {
            result_response(session_prune(dry_run, ttl_secs))
        }
        Action::MenuList { app, depth } => {
            let app = match app.or_else(|| context.app.clone()) {
                Some(app) => app,
                None => return Response::from_error(invalid_arguments("menu list requires --app")),
            };
            let depth = depth.unwrap_or(MENU_DEFAULT_DEPTH);
            result_response(menu_list(backend, &app, depth))
        }
        Action::MenuClick { app, path } => {
            let app = match app.or_else(|| context.app.clone()) {
                Some(app) => app,
                None => {
                    return Response::from_error(invalid_arguments("menu click requires --app"))
                }
            };
            if path.trim().is_empty() {
                return Response::from_error(invalid_arguments("menu click requires --path"));
            }
            result_response(menu_click(backend, &app, &path))
        }
        Action::AppleEvent {
            app,
            script,
            language,
        } => {
            let app = match app.or_else(|| context.app.clone()) {
                Some(app) => app,
                None => {
                    return Response::from_error(invalid_arguments("apple-event requires --app"))
                }
            };
            if script.is_empty() {
                return Response::from_error(invalid_arguments("apple-event requires --script"));
            }
            let language = language.unwrap_or_default();
            result_response(apple_event_action(backend, &app, &script, language))
        }
        Action::ShortcutsRun { name } => {
            if !cfg!(target_os = "macos") {
                return Response::from_error(unsupported_error("shortcuts_run"));
            }
            if name.trim().is_empty() {
                return Response::from_error(invalid_arguments(
                    "shortcuts run requires a workflow name",
                ));
            }
            result_response(shortcuts_run_action(name))
        }
        Action::DockList {} => result_response(dock_list_action(backend)),
        Action::DockClick { title } => {
            if title.trim().is_empty() {
                return Response::from_error(invalid_arguments("dock click requires --title"));
            }
            result_response(dock_click_action(backend, &title))
        }
        Action::Spaces {} => result_response(spaces_action(backend)),
        Action::SpacesMove { to_space } => result_response(spaces_move_action(to_space)),
        other if is_browser_action(&other) => {
            let response = match browser_command(&other, context) {
                Ok(command) => match browser_options(context, &prepared.subject) {
                    Ok(options) => result_response(crate::browser::run(command, &options)),
                    Err(error) => Response::from_error(error),
                },
                Err(error) => Response::from_error(error),
            };
            let response = attach_post_state(backend, context, kind, response);
            return finish_action(
                context,
                label,
                &prepared.subject,
                prepared.warning.as_deref(),
                prepared.decision,
                prepared.source,
                chars,
                response,
            );
        }
        Action::PolicyCheck => {
            let (response, subject) = policy_check(backend, context);
            let response = attach_post_state(backend, context, kind, response);
            return finish_action(
                context,
                label,
                &subject,
                prepared.warning.as_deref(),
                prepared.decision,
                prepared.source,
                chars,
                response,
            );
        }
        Action::Pause => pause_response(true),
        Action::Resume => pause_response(false),
        other => Response::from_error(ComputerError::InvalidArguments(format!(
            "unsupported action in this build: {other:?}"
        ))),
    };
    let response = attach_post_state(backend, context, kind, response);
    finish_action(
        context,
        label,
        &prepared.subject,
        prepared.warning.as_deref(),
        prepared.decision,
        prepared.source,
        chars,
        response,
    )
}

#[allow(clippy::too_many_arguments)]
fn finish_action(
    context: &CallContext,
    label: &str,
    subject: &policy::Subject,
    warning: Option<&str>,
    decision: &str,
    source: &str,
    chars: Option<u64>,
    mut response: Response,
) -> Response {
    if let Some(warning) = warning {
        push_warning(&mut response, warning);
    }
    let code = if response.ok {
        "ok"
    } else {
        response
            .error
            .as_ref()
            .map(|error| error.code.as_str())
            .unwrap_or("execution_failed")
    };
    let mut decision = decision.to_string();
    let mut source = source.to_string();
    if label == "policy_check" {
        if let Some(value) = response
            .fields
            .get("decision")
            .and_then(|value| value.as_str())
        {
            decision = value.to_string();
        }
        if let Some(value) = response
            .fields
            .get("source")
            .and_then(|value| value.as_str())
        {
            source = value.to_string();
        }
    }
    let tier = response
        .fields
        .get("tier")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let session = context
        .session
        .clone()
        .unwrap_or_else(|| std::process::id().to_string());
    if let Err(error) = crate::audit::record(&crate::audit::AuditRecord {
        session: &session,
        action: label,
        bundle_id: subject.bundle_id.as_deref(),
        pid: subject.pid,
        window_id: subject.window_id,
        element: context.element,
        code,
        decision: &decision,
        source: &source,
        tier: tier.as_deref(),
        chars,
    }) {
        push_warning(
            &mut response,
            &format!("audit log was not written: {error}"),
        );
    }
    response
}

fn push_warning(response: &mut Response, warning: &str) {
    let warnings = response
        .fields
        .entry("warnings")
        .or_insert_with(|| json!([]));
    if let Some(items) = warnings.as_array_mut() {
        items.push(json!(warning));
    }
}

fn action_label(action: &Action) -> &'static str {
    match action {
        Action::Screenshot { .. } => "screenshot",
        Action::Zoom { .. } => "zoom",
        Action::Click { .. } => "click",
        Action::DoubleClick { .. } => "double_click",
        Action::TripleClick { .. } => "triple_click",
        Action::Press { .. } => "press",
        Action::Drag { .. } => "drag",
        Action::MouseDown { .. } => "mouse_down",
        Action::MouseUp { .. } => "mouse_up",
        Action::HoldKey { .. } => "hold_key",
        Action::ClipboardGet => "clipboard_get",
        Action::ClipboardSet { .. } => "clipboard_set",
        Action::Focus => "focus",
        Action::SetValue { .. } => "set_value",
        Action::SelectText { .. } => "select_text",
        Action::PerformAction { .. } => "perform_action",
        Action::ScrollTo => "scroll_to",
        Action::Move { .. } => "move",
        Action::Type { .. } => "type",
        Action::Key { .. } => "key",
        Action::Scroll { .. } => "scroll",
        Action::GetState => "get_state",
        Action::Wait { .. } => "wait",
        Action::Verify { .. } => "verify",
        Action::Displays => "displays",
        Action::Isolation => "isolation",
        Action::ListApps { .. } => "list_apps",
        Action::ListWindows { .. } => "list_windows",
        Action::Launch => "launch",
        Action::Activate => "activate",
        Action::Quit { .. } => "quit",
        Action::Doctor { .. } => "doctor",
        Action::Tree => "tree",
        Action::SessionEnd => "session_end",
        Action::AuditPath {} => "audit_path",
        Action::AuditTail { .. } => "audit_tail",
        Action::CursorPosition => "cursor_position",
        Action::Batch { .. } => "batch",
        Action::SessionPrune { .. } => "session_prune",
        Action::PolicyCheck => "policy_check",
        Action::Pause => "pause",
        Action::Resume => "resume",
        Action::MenuList { .. } => "menu_list",
        Action::MenuClick { .. } => "menu_click",
        Action::AppleEvent { .. } => "apple_event",
        Action::ShortcutsRun { .. } => "shortcuts_run",
        Action::DockList { .. } => "dock_list",
        Action::DockClick { .. } => "dock_click",
        Action::Spaces { .. } => "spaces",
        Action::SpacesMove { .. } => "spaces_move",
        Action::BrowserTabs { .. } => "browser_tabs",
        Action::BrowserActivate { .. } => "browser_activate",
        Action::BrowserNavigate { .. } => "browser_navigate",
        Action::BrowserBack { .. } => "browser_back",
        Action::BrowserForward { .. } => "browser_forward",
        Action::BrowserReload { .. } => "browser_reload",
        Action::BrowserWait { .. } => "browser_wait",
        Action::BrowserSnapshot { .. } => "browser_snapshot",
        Action::BrowserClick { .. } => "browser_click",
        Action::BrowserText { .. } => "browser_text",
        Action::BrowserType { .. } => "browser_type",
        Action::BrowserPress { .. } => "browser_press",
        Action::BrowserHover { .. } => "browser_hover",
        Action::BrowserScroll { .. } => "browser_scroll",
        Action::BrowserSelect { .. } => "browser_select",
        Action::BrowserCheck { .. } => "browser_check",
        Action::BrowserUpload { .. } => "browser_upload",
        Action::BrowserDialog { .. } => "browser_dialog",
        Action::BrowserScreenshot { .. } => "browser_screenshot",
        Action::BrowserEval { .. } => "browser_eval",
        Action::BrowserTabNew { .. } => "browser_tab_new",
        Action::BrowserTabClose { .. } => "browser_tab_close",
        Action::BrowserLaunch { .. } => "browser_launch",
        Action::BrowserQuit { .. } => "browser_quit",
        Action::BrowserSessions {} => "browser_sessions",
        Action::BrowserBatch { .. } => "browser_batch",
        Action::Ocr { .. } => "ocr",
        Action::WindowMove { .. } => "window_move",
        Action::WindowResize { .. } => "window_resize",
        Action::WindowSet { .. } => "window_set",
        Action::WindowState { .. } => "window_state",
        Action::WindowInfo => "window_info",
    }
}

#[derive(Clone, Copy)]
enum PolicyClass {
    Always,
    Benign,
    Observe,
    Mutate,
    Quit,
    Key,
    ClipboardSet,
    Script,
}

fn policy_class(action: &Action) -> PolicyClass {
    match action {
        Action::Wait { .. }
        | Action::Doctor { .. }
        | Action::Displays
        | Action::Isolation
        | Action::ListApps { .. }
        | Action::ListWindows { .. }
        | Action::Pause
        | Action::Resume => PolicyClass::Always,
        Action::SessionEnd
        | Action::SessionPrune { .. }
        | Action::CursorPosition
        | Action::AuditPath {}
        | Action::AuditTail { .. } => PolicyClass::Benign,
        Action::Screenshot { .. }
        | Action::Zoom { .. }
        | Action::Tree
        | Action::Verify { .. }
        | Action::GetState
        | Action::PolicyCheck
        | Action::MenuList { .. }
        | Action::DockList { .. }
        | Action::Spaces { .. }
        | Action::BrowserTabs { .. }
        | Action::BrowserText { .. }
        | Action::BrowserSnapshot { .. }
        | Action::BrowserScreenshot { .. }
        | Action::BrowserWait { .. }
        | Action::BrowserSessions {}
        | Action::BrowserActivate { .. }
        | Action::WindowInfo
        | Action::WindowState {
            minimized: None,
            fullscreen: None,
        }
        | Action::Ocr { .. } => PolicyClass::Observe,
        Action::Quit { .. } => PolicyClass::Quit,
        Action::Key { .. } | Action::HoldKey { .. } => PolicyClass::Key,
        Action::ClipboardGet => PolicyClass::Benign,
        Action::ClipboardSet { .. } => PolicyClass::ClipboardSet,
        Action::AppleEvent { .. } | Action::ShortcutsRun { .. } => PolicyClass::Script,
        _ => PolicyClass::Mutate,
    }
}

struct Prepared {
    stop: Option<Response>,
    warning: Option<String>,
    subject: policy::Subject,
    decision: &'static str,
    source: &'static str,
}

fn early_stop(warning: Option<String>, subject: policy::Subject, response: Response) -> Prepared {
    Prepared {
        stop: Some(response),
        warning,
        subject,
        decision: "allow",
        source: "default",
    }
}

fn prepare_action(
    backend: &dyn DesktopBackend,
    action: &Action,
    context: &CallContext,
) -> Prepared {
    let class = policy_class(action);
    let loaded = match policy::load_policy(context.policy.as_deref().map(Path::new)) {
        Ok(policy) => policy,
        Err(error) => {
            if matches!(class, PolicyClass::Always) {
                policy::LoadedPolicy::default()
            } else {
                return early_stop(
                    None,
                    policy::Subject::default(),
                    Response::from_error(error),
                );
            }
        }
    };
    // The backends consult the built-in deny list without carrying the policy,
    // so the exemptions go into the process-wide registry here.
    policy::set_builtin_exemptions(loaded.builtin_exempt());
    policy::set_window_clamp(loaded.window_clamp());
    crate::audit::set_config(crate::audit::AuditConfig {
        enabled: loaded.audit_enabled(),
        max_bytes: loaded.audit_max_bytes(),
        keep: loaded.audit_keep(),
    });
    let mut warning = if loaded.pause_on_user_input {
        backend.user_input_pause_warning().map(str::to_string)
    } else {
        None
    };
    if matches!(action, Action::Pause | Action::Resume | Action::PolicyCheck) {
        return Prepared {
            stop: None,
            warning,
            subject: policy::Subject::default(),
            decision: "allow",
            source: "default",
        };
    }
    let subject = match resolve_subject(backend, action, context) {
        Ok(subject) => subject,
        Err(error) => {
            return early_stop(
                warning,
                policy::Subject::default(),
                Response::from_error(error),
            )
        }
    };
    let policy_action = match action {
        Action::Quit { .. } => policy::PolicyAction::Quit,
        Action::Key { keys } | Action::HoldKey { keys, .. } => policy::PolicyAction::Key {
            combo: keys.clone(),
        },
        Action::Screenshot { .. } | Action::Zoom { .. } | Action::Tree | Action::GetState => {
            policy::PolicyAction::Observe
        }
        Action::Ocr { .. } => policy::PolicyAction::Observe,
        Action::ClipboardSet { .. } => policy::PolicyAction::ClipboardSet,
        _ => match class {
            PolicyClass::Always => policy::PolicyAction::Always,
            PolicyClass::Benign => policy::PolicyAction::Benign,
            PolicyClass::Mutate => policy::PolicyAction::Mutate,
            PolicyClass::Observe => policy::PolicyAction::Observe,
            PolicyClass::Quit => policy::PolicyAction::Quit,
            PolicyClass::Key => policy::PolicyAction::Key {
                combo: String::new(),
            },
            PolicyClass::ClipboardSet => policy::PolicyAction::ClipboardSet,
            PolicyClass::Script => policy::PolicyAction::Script,
        },
    };
    let decision = policy::decide(&loaded, &subject, &policy_action);
    if let policy::Decision::Deny { message, source } = &decision {
        return Prepared {
            stop: Some(Response::from_error(ComputerError::PolicyDenied(
                message.clone(),
            ))),
            warning,
            subject,
            decision: "deny",
            source,
        };
    }
    if let Err(error) = refuse_blocked_url(backend, &loaded, &subject) {
        let (decision_name, source) = decision_record(&decision);
        return Prepared {
            stop: Some(Response::from_error(error)),
            warning,
            subject,
            decision: decision_name,
            source,
        };
    }
    let mutating = matches!(
        class,
        PolicyClass::Mutate
            | PolicyClass::Quit
            | PolicyClass::Key
            | PolicyClass::ClipboardSet
            | PolicyClass::Script
    );
    if mutating && policy::is_paused() {
        let (decision_name, source) = decision_record(&decision);
        return Prepared {
            stop: Some(Response::from_error(ComputerError::Paused(
                "local input paused mutating actions; run computer-use resume".into(),
            ))),
            warning,
            subject,
            decision: decision_name,
            source,
        };
    }
    if let Action::Key { .. } | Action::HoldKey { .. } = action {
        if let Some(index) = context.element {
            if let Ok(Some(peek)) = backend.peek_element(
                context.app.as_deref(),
                context.window.as_deref(),
                context.session.as_deref(),
                index,
            ) {
                if peek.secure {
                    return Prepared {
                        stop: Some(Response::from_error(ComputerError::PolicyDenied(
                            "refusing a key event aimed at a secure field".into(),
                        ))),
                        warning,
                        subject,
                        decision: "deny",
                        source: "builtin",
                    };
                }
            }
        }
    }
    if let Action::MenuClick { path, .. } = action {
        if menu_needs_confirm(path) {
            if let Err(error) = confirm_action(
                context,
                &loaded,
                &subject,
                action,
                "sensitive menu item; confirm the exact path",
            ) {
                return Prepared {
                    stop: Some(Response::from_error(error)),
                    warning,
                    subject,
                    decision: "confirm",
                    source: "builtin",
                };
            }
        }
    }
    if context.element.is_none() && action_inserts_text(action) {
        match backend.peek_focused(context.app.as_deref(), context.window.as_deref()) {
            Ok(Some(peek)) if peek.secure => match loaded.secure_focus() {
                policy::FocusDecision::Deny => {
                    return Prepared {
                        stop: Some(Response::from_error(ComputerError::PolicyDenied(
                            "the focused element is a secure field; ask the user to type it".into(),
                        ))),
                        warning,
                        subject,
                        decision: "deny",
                        source: "builtin",
                    };
                }
                policy::FocusDecision::Warn => {
                    let note = "focused element is a secure field; proceeding because [security] secure_focus = warn";
                    warning = Some(append_warning(warning, note));
                }
            },
            Ok(_) => {}
            Err(error) => {
                let note = format!("secure-field check unavailable: {error}");
                match loaded.unreadable_focus() {
                    policy::FocusDecision::Deny => {
                        return Prepared {
                            stop: Some(Response::from_error(ComputerError::PolicyDenied(format!(
                                "{note}; set [security] unreadable_focus = warn to proceed"
                            )))),
                            warning,
                            subject,
                            decision: "deny",
                            source: "builtin",
                        };
                    }
                    policy::FocusDecision::Warn => {
                        warning = Some(append_warning(warning, &note));
                    }
                }
            }
        }
    }
    if let policy::Decision::Confirm { reason } = &decision {
        if let Err(error) = confirm_action(context, &loaded, &subject, action, reason) {
            return Prepared {
                stop: Some(Response::from_error(error)),
                warning,
                subject,
                decision: "confirm",
                source: "builtin",
            };
        }
    }
    let (decision_name, source) = decision_record(&decision);
    Prepared {
        stop: None,
        warning,
        subject,
        decision: decision_name,
        source,
    }
}

fn decision_record(decision: &policy::Decision) -> (&'static str, &'static str) {
    match decision {
        policy::Decision::Allow { source } => ("allow", *source),
        policy::Decision::Deny { source, .. } => ("deny", *source),
        policy::Decision::Confirm { .. } => ("confirm", "builtin"),
    }
}

fn append_warning(existing: Option<String>, note: &str) -> String {
    match existing {
        Some(text) if !text.is_empty() => format!("{text}; {note}"),
        _ => note.to_string(),
    }
}

fn menu_needs_confirm(path: &str) -> bool {
    crate::menu_path::parse(path)
        .map(|parsed| {
            parsed
                .as_slice()
                .iter()
                .any(|segment| policy::sensitive_title(segment))
        })
        .unwrap_or(false)
}

fn action_inserts_text(action: &Action) -> bool {
    match action {
        Action::Type { .. } => true,
        Action::Key { keys } | Action::HoldKey { keys, .. } => combo_inserts_text(keys),
        _ => false,
    }
}

fn combo_inserts_text(keys: &str) -> bool {
    let Ok(combos) = crate::keys::parse_key_sequence(keys) else {
        return false;
    };
    combos.iter().any(|combo| {
        use crate::types::Modifier;
        let key = combo.key.to_ascii_lowercase();
        let has = |modifier: Modifier| combo.modifiers.contains(&modifier);
        let command = has(Modifier::Command) || has(Modifier::Meta);
        let control = has(Modifier::Control);
        let shift = has(Modifier::Shift);
        if key == "v" && command {
            return true;
        }
        if key == "insert" && shift {
            return true;
        }
        key.chars().count() == 1 && !command && !control
    })
}

fn confirm_action(
    context: &CallContext,
    loaded: &policy::LoadedPolicy,
    subject: &policy::Subject,
    action: &Action,
    reason: &str,
) -> Result<(), ComputerError> {
    let command = policy::confirm_program(loaded, context.confirm_cmd.as_deref());
    policy::run_confirm(
        command.as_deref(),
        loaded.timeout_secs(),
        &policy::ConfirmDocument {
            action: action_label(action),
            bundle_id: subject.bundle_id.as_deref(),
            path: subject.path.as_deref(),
            pid: subject.pid,
            window_id: subject.window_id,
            element: context.element,
            reason,
            extra: confirm_extra(action),
        },
    )
}

/// Action-specific detail for the confirm hook. Never the script body.
fn confirm_extra(action: &Action) -> Option<Value> {
    match action {
        Action::AppleEvent {
            script, language, ..
        } => Some(json!({
            "script_bytes": script.len(),
            "script_hash": policy::script_hash(script.as_bytes()),
            "language": language.unwrap_or_default().as_str(),
        })),
        Action::ShortcutsRun { name } => Some(json!({"name": name})),
        Action::MenuClick { path, .. } => Some(json!({"path": path})),
        Action::Key { keys } | Action::HoldKey { keys, .. } => Some(json!({"combo": keys})),
        _ => None,
    }
}

fn refuse_blocked_url(
    backend: &dyn DesktopBackend,
    loaded: &policy::LoadedPolicy,
    subject: &policy::Subject,
) -> Result<(), ComputerError> {
    if loaded.blocked_urls().is_empty() {
        return Ok(());
    }
    if !policy::is_browser(
        subject.bundle_id.as_deref(),
        subject.path.as_deref().map(Path::new),
        subject.name.as_deref(),
    ) {
        return Ok(());
    }
    let from_title = subject
        .window_title
        .as_deref()
        .and_then(policy::url_in_text)
        .map(str::to_string);
    let url = if let Some(url) = from_title {
        Some(url)
    } else if let Some(pid) = subject.pid {
        backend.browser_url(pid)?
    } else {
        None
    };
    if let Some(url) = url.as_deref() {
        if let Some(pattern) = policy::url_blocked(url, loaded.blocked_urls()) {
            return Err(ComputerError::BlockedUrl(format!("matched {pattern}")));
        }
    }
    Ok(())
}

fn resolve_subject(
    backend: &dyn DesktopBackend,
    action: &Action,
    context: &CallContext,
) -> Result<policy::Subject, ComputerError> {
    let class = policy_class(action);
    if matches!(class, PolicyClass::Always | PolicyClass::Benign) {
        return Ok(policy::Subject::default());
    }
    let screenshot = matches!(action, Action::Screenshot { .. } | Action::Zoom { .. });
    let frontmost_observe =
        matches!(action, Action::Tree | Action::GetState) && context.app.is_none();
    let app_text = context.app.clone().or_else(|| {
        if frontmost_observe {
            backend.frontmost().app
        } else {
            None
        }
    });
    if app_text.is_none() && context.window.is_none() {
        return Ok(policy::Subject::default());
    }
    if app_text.is_none() && screenshot {
        return subject_from_window(backend, context);
    }
    let Some(app_text) = app_text else {
        return Ok(policy::Subject::default());
    };
    let selector = parse_app_selector(&app_text)?;
    let fallback = subject_from_selector(&selector);
    let apps = match backend.list_apps(true) {
        Ok(apps) => apps,
        Err(error) => {
            if policy::selector_denial(&selector).is_some() || matches!(action, Action::Launch) {
                return Ok(fallback);
            }
            return Err(error);
        }
    };
    let info = match resolve_app(&apps, &selector) {
        Ok(info) => info.clone(),
        Err(error) => {
            if policy::selector_denial(&selector).is_some() || matches!(action, Action::Launch) {
                return Ok(fallback);
            }
            // An implicit frontmost name that is not running is not a target.
            // The backend resolves the key window itself.
            if context.app.is_none() {
                return Ok(policy::Subject::default());
            }
            return Err(error);
        }
    };
    let mut subject = policy::Subject {
        name: Some(info.name.clone()),
        bundle_id: info.bundle_id.clone(),
        path: info.path.clone(),
        pid: Some(info.pid),
        window_id: None,
        window_title: None,
        ax_identifier: None,
    };
    let want_window = context.window.is_some()
        || matches!(
            action,
            Action::Tree | Action::GetState | Action::Screenshot { .. } | Action::Zoom { .. }
        );
    if want_window {
        if let Ok(listed) = backend.list_windows() {
            let owned = filter_windows_for_pid(&listed.windows, info.pid);
            let chosen = match context.window.as_deref() {
                Some(window) => {
                    let selector = parse_window_selector(window)?;
                    Some(resolve_window(&owned, &selector)?.clone())
                }
                None => owned.first().cloned(),
            };
            if let Some(window) = chosen {
                subject.window_id = Some(window.id);
                if context.window.is_some()
                    || matches!(
                        action,
                        Action::Tree
                            | Action::GetState
                            | Action::Screenshot { .. }
                            | Action::Zoom { .. }
                    )
                {
                    subject.window_title = Some(window.title);
                }
            }
        }
    }
    Ok(subject)
}

fn subject_from_window(
    backend: &dyn DesktopBackend,
    context: &CallContext,
) -> Result<policy::Subject, ComputerError> {
    let Some(window) = context.window.as_deref() else {
        return Ok(policy::Subject::default());
    };
    let listed = backend.list_windows()?;
    let selector = parse_window_selector(window)?;
    let window = resolve_window(&listed.windows, &selector)?.clone();
    let apps = backend.list_apps(true)?;
    let info = apps.into_iter().find(|app| app.pid == window.pid);
    Ok(policy::Subject {
        name: info
            .as_ref()
            .map(|app| app.name.clone())
            .or(Some(window.app.clone())),
        bundle_id: info.as_ref().and_then(|app| app.bundle_id.clone()),
        path: info.and_then(|app| app.path),
        pid: Some(window.pid),
        window_id: Some(window.id),
        window_title: Some(window.title),
        ax_identifier: None,
    })
}

fn subject_from_selector(selector: &crate::target::AppSelector) -> policy::Subject {
    use crate::target::AppSelector;
    match selector {
        AppSelector::BundleId(bundle) => policy::Subject {
            bundle_id: Some(bundle.clone()),
            ..policy::Subject::default()
        },
        AppSelector::Name(name) => policy::Subject {
            name: Some(name.clone()),
            ..policy::Subject::default()
        },
        AppSelector::Path(path) => policy::Subject {
            path: Some(path.display().to_string()),
            ..policy::Subject::default()
        },
        AppSelector::Pid(pid) => policy::Subject {
            pid: Some(*pid),
            ..policy::Subject::default()
        },
    }
}

fn policy_check(
    backend: &dyn DesktopBackend,
    context: &CallContext,
) -> (Response, policy::Subject) {
    let Some(app) = context
        .app
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    else {
        return (
            Response::from_error(ComputerError::InvalidArguments("app is required".into())),
            policy::Subject::default(),
        );
    };
    let loaded = match policy::load_policy(context.policy.as_deref().map(Path::new)) {
        Ok(policy) => policy,
        Err(error) => return (Response::from_error(error), policy::Subject::default()),
    };
    policy::set_builtin_exemptions(loaded.builtin_exempt());
    let selector = match parse_app_selector(app) {
        Ok(selector) => selector,
        Err(error) => return (Response::from_error(error), policy::Subject::default()),
    };
    let mut subject = subject_from_selector(&selector);
    if let Ok(apps) = backend.list_apps(true) {
        if let Ok(info) = resolve_app(&apps, &selector) {
            subject.name = Some(info.name.clone());
            subject.bundle_id = info.bundle_id.clone();
            subject.path = info.path.clone();
            subject.pid = Some(info.pid);
        }
    }
    let decision = policy::app_verdict(&loaded, &subject);
    let (name, source, code) = match decision {
        policy::Decision::Allow { source } => ("allow", source, "ok"),
        policy::Decision::Deny { source, .. } => ("deny", source, "policy_denied"),
        policy::Decision::Confirm { .. } => ("allow", "default", "ok"),
    };
    (
        Response::success(json!({
            "decision": name,
            "source": source,
            "code": code,
            "bundle_id": subject.bundle_id,
        })),
        subject,
    )
}

fn pause_response(paused: bool) -> Response {
    match policy::set_paused(paused) {
        Ok(()) => Response::success(json!({
            "action": if paused { "pause" } else { "resume" },
            "paused": paused,
        })),
        Err(error) => Response::from_error(error),
    }
}

fn result_response(result: Result<Value, ComputerError>) -> Response {
    match result {
        Ok(value) => Response::success(value),
        Err(error) => Response::from_error(error),
    }
}

fn post_state_kind(action: &Action) -> PostKind {
    match action {
        Action::Screenshot { .. } | Action::Zoom { .. } => PostKind::Screenshot,
        Action::Click { .. }
        | Action::DoubleClick { .. }
        | Action::TripleClick { .. }
        | Action::Drag { .. }
        | Action::MouseDown { .. }
        | Action::MouseUp { .. }
        | Action::HoldKey { .. }
        | Action::ClipboardSet { .. }
        | Action::Press { .. }
        | Action::Focus
        | Action::SetValue { .. }
        | Action::SelectText { .. }
        | Action::PerformAction { .. }
        | Action::ScrollTo
        | Action::Move { .. }
        | Action::Type { .. }
        | Action::Key { .. }
        | Action::Scroll { .. }
        | Action::Launch
        | Action::Activate
        | Action::Quit { .. }
        | Action::Batch { .. }
        | Action::MenuClick { .. }
        | Action::AppleEvent { .. }
        | Action::ShortcutsRun { .. }
        | Action::DockClick { .. }
        | Action::BrowserNavigate { .. }
        | Action::BrowserBack { .. }
        | Action::BrowserForward { .. }
        | Action::BrowserReload { .. }
        | Action::BrowserClick { .. }
        | Action::BrowserType { .. }
        | Action::BrowserPress { .. }
        | Action::BrowserHover { .. }
        | Action::BrowserScroll { .. }
        | Action::BrowserSelect { .. }
        | Action::BrowserCheck { .. }
        | Action::BrowserUpload { .. }
        | Action::BrowserDialog { .. }
        | Action::BrowserEval { .. }
        | Action::BrowserActivate { .. }
        | Action::BrowserTabNew { .. }
        | Action::BrowserTabClose { .. }
        | Action::BrowserLaunch { .. }
        | Action::BrowserQuit { .. }
        | Action::BrowserBatch { .. }
        | Action::WindowMove { .. }
        | Action::WindowResize { .. }
        | Action::WindowSet { .. }
        | Action::WindowState {
            minimized: Some(_), ..
        }
        | Action::WindowState {
            fullscreen: Some(_),
            ..
        }
        | Action::SpacesMove { .. }
        | Action::Ocr { .. } => PostKind::Mutating,
        Action::GetState
        | Action::Wait { .. }
        | Action::Verify { .. }
        | Action::Displays
        | Action::Isolation
        | Action::ListApps { .. }
        | Action::ListWindows { .. }
        | Action::Doctor { .. }
        | Action::Tree
        | Action::SessionEnd
        | Action::SessionPrune { .. }
        | Action::AuditPath {}
        | Action::AuditTail { .. }
        | Action::CursorPosition
        | Action::ClipboardGet
        | Action::PolicyCheck
        | Action::Pause
        | Action::Resume
        | Action::MenuList { .. }
        | Action::DockList { .. }
        | Action::Spaces { .. }
        | Action::BrowserTabs { .. }
        | Action::BrowserText { .. }
        | Action::BrowserSnapshot { .. }
        | Action::BrowserScreenshot { .. }
        | Action::BrowserWait { .. }
        | Action::WindowInfo
        | Action::WindowState {
            minimized: None,
            fullscreen: None,
        }
        | Action::BrowserSessions {} => PostKind::ReadOnly,
    }
}

fn remap_coordinates(context: &CallContext, action: Action) -> Result<Action, ComputerError> {
    match context.coord.as_deref() {
        None | Some("screen") => Ok(action),
        Some("image") => remap_image_action(context, action),
        Some(other) => Err(ComputerError::InvalidArguments(format!(
            "coord must be screen or image (got {other})"
        ))),
    }
}

fn remap_image_action(context: &CallContext, action: Action) -> Result<Action, ComputerError> {
    match action {
        Action::Click {
            x,
            y,
            button,
            double,
            count,
        } => {
            let (x, y) = remap_optional_point(context, x, y)?;
            Ok(Action::Click {
                x,
                y,
                button,
                double,
                count,
            })
        }
        Action::DoubleClick { x, y, button } => {
            let (x, y) = remap_optional_point(context, x, y)?;
            Ok(Action::DoubleClick { x, y, button })
        }
        Action::TripleClick { x, y, button } => {
            let (x, y) = remap_optional_point(context, x, y)?;
            Ok(Action::TripleClick { x, y, button })
        }
        Action::Drag { path, button } => {
            let mut mapped = Vec::with_capacity(path.len());
            for point in path {
                let (x, y) = remap_point(context, point.x, point.y)?;
                mapped.push(Point { x, y });
            }
            Ok(Action::Drag {
                path: mapped,
                button,
            })
        }
        Action::MouseDown { x, y, button } => {
            let (x, y) = remap_point(context, x, y)?;
            Ok(Action::MouseDown { x, y, button })
        }
        Action::MouseUp { x, y, button } => {
            let (x, y) = remap_point(context, x, y)?;
            Ok(Action::MouseUp { x, y, button })
        }
        Action::Move { x, y } => {
            let (x, y) = remap_point(context, x, y)?;
            Ok(Action::Move { x, y })
        }
        Action::Scroll {
            x,
            y,
            dx,
            dy,
            direction,
            amount,
        } => {
            let (x, y) = remap_optional_point(context, x, y)?;
            Ok(Action::Scroll {
                x,
                y,
                dx,
                dy,
                direction,
                amount,
            })
        }
        Action::Screenshot {
            output,
            region,
            format,
            max_width,
            canvas,
            base64,
            marks,
        } => {
            let region = match region {
                Some(region) => Some(remap_region(context, region)?),
                None => None,
            };
            Ok(Action::Screenshot {
                output,
                region,
                format,
                max_width,
                canvas,
                base64,
                marks,
            })
        }
        Action::Zoom {
            output,
            region,
            format,
            max_width,
            canvas,
            base64,
        } => {
            let region = remap_region(context, region)?;
            Ok(Action::Zoom {
                output,
                region,
                format,
                max_width,
                canvas,
                base64,
            })
        }
        other => Ok(other),
    }
}

/// Resolve `--display` once per action, then check every screen point it names.
///
/// Runs after [`remap_coordinates`], so `--coord image` has already become a
/// screen point and the filter sees the same space the click will use. Points
/// stay virtual-desktop points: `--display` narrows what may be addressed, it
/// does not rebase them.
fn gate_display(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    action: &Action,
) -> Result<(), ComputerError> {
    let Some(spec) = context.display.as_deref() else {
        return Ok(());
    };
    // Syntax is checked for every action, so garbage never reaches the backend.
    // `virtual` is a CLI-only keyword: a JSON request cannot exec a prepared
    // X server, so it gets a structured `unsupported` instead of a silent
    // whole-desktop capture.
    crate::isolation::parse_selector(spec)?;
    // `displays` and `isolation` report on filtering, so they do not consume it.
    if matches!(action, Action::Isolation | Action::Displays) {
        return Ok(());
    }
    let displays = backend.list_displays()?;
    let target = crate::isolation::resolve(spec, &displays)?;
    match action {
        // A click without a point is element-targeted, so there is nothing to
        // filter: the element already resolved inside its window.
        Action::Click { x, y, .. }
        | Action::DoubleClick { x, y, .. }
        | Action::TripleClick { x, y, .. }
        | Action::Scroll { x, y, .. } => match (x, y) {
            (Some(x), Some(y)) => crate::isolation::ensure_point_inside(&target, *x, *y),
            _ => Ok(()),
        },
        Action::Move { x, y } | Action::MouseDown { x, y, .. } | Action::MouseUp { x, y, .. } => {
            crate::isolation::ensure_point_inside(&target, *x, *y)
        }
        Action::Drag { path, .. } => {
            for point in path {
                crate::isolation::ensure_point_inside(&target, point.x, point.y)?;
            }
            Ok(())
        }
        Action::Screenshot {
            region: Some(region),
            ..
        } => crate::isolation::ensure_region_inside(&target, *region),
        Action::Screenshot { .. } => Ok(()),
        Action::Zoom { region, .. } => crate::isolation::ensure_region_inside(&target, *region),
        _ => Ok(()),
    }
}

fn remap_optional_point(
    context: &CallContext,
    x: Option<i32>,
    y: Option<i32>,
) -> Result<(Option<i32>, Option<i32>), ComputerError> {
    match (x, y) {
        (Some(x), Some(y)) => {
            let (x, y) = remap_point(context, x, y)?;
            Ok((Some(x), Some(y)))
        }
        other => Ok(other),
    }
}

fn remap_point(context: &CallContext, x: i32, y: i32) -> Result<(i32, i32), ComputerError> {
    let map = require_screenshot_map(context)?;
    ensure_point_inside(&map, x, y)?;
    let (screen_x, screen_y) = image_util::image_to_screen(&map, f64::from(x), f64::from(y));
    Ok((round_screen(screen_x)?, round_screen(screen_y)?))
}

fn remap_region(context: &CallContext, region: Region) -> Result<Region, ComputerError> {
    let map = require_screenshot_map(context)?;
    ensure_region_inside(&map, region)?;
    let x2 = i64::from(region.x) + i64::from(region.width);
    let y2 = i64::from(region.y) + i64::from(region.height);
    let (screen_x1, screen_y1) =
        image_util::image_to_screen(&map, f64::from(region.x), f64::from(region.y));
    let (screen_x2, screen_y2) = image_util::image_to_screen(&map, x2 as f64, y2 as f64);
    let x = round_screen(screen_x1)?;
    let y = round_screen(screen_y1)?;
    let far_x = round_screen(screen_x2)?;
    let far_y = round_screen(screen_y2)?;
    let width = u32::try_from(i64::from(far_x) - i64::from(x))
        .unwrap_or(0)
        .max(1);
    let height = u32::try_from(i64::from(far_y) - i64::from(y))
        .unwrap_or(0)
        .max(1);
    Region::new(x, y, width, height)
}

fn require_screenshot_map(
    context: &CallContext,
) -> Result<image_util::ScreenshotMap, ComputerError> {
    match cache::load_screenshot_map(context.session.as_deref())? {
        Some(map) => Ok(map),
        None => Err(ComputerError::InvalidArguments(
            "take a screenshot first".into(),
        )),
    }
}

fn content_box(map: &image_util::ScreenshotMap) -> (i64, i64, i64, i64) {
    let left = i64::from(map.pad.x);
    let top = i64::from(map.pad.y);
    let right = left + i64::from(map.content_width);
    let bottom = top + i64::from(map.content_height);
    (left, top, right, bottom)
}

fn ensure_point_inside(
    map: &image_util::ScreenshotMap,
    x: i32,
    y: i32,
) -> Result<(), ComputerError> {
    let (left, top, right, bottom) = content_box(map);
    let x = i64::from(x);
    let y = i64::from(y);
    if x < left || y < top || x >= right || y >= bottom {
        return Err(ComputerError::InvalidArguments(format!(
            "coordinate ({x}, {y}) is outside the screenshot"
        )));
    }
    Ok(())
}

fn ensure_region_inside(
    map: &image_util::ScreenshotMap,
    region: Region,
) -> Result<(), ComputerError> {
    let (left, top, right, bottom) = content_box(map);
    let x1 = i64::from(region.x);
    let y1 = i64::from(region.y);
    let x2 = x1 + i64::from(region.width);
    let y2 = y1 + i64::from(region.height);
    if x1 < left || y1 < top || x1 >= right || y1 >= bottom || x2 > right || y2 > bottom {
        return Err(ComputerError::InvalidArguments(format!(
            "region {},{} {}x{} is outside the screenshot",
            region.x, region.y, region.width, region.height
        )));
    }
    Ok(())
}

fn round_screen(value: f64) -> Result<i32, ComputerError> {
    if !value.is_finite() {
        return Err(ComputerError::InvalidArguments(
            "coordinate remap was not finite".into(),
        ));
    }
    let rounded = value.round();
    if rounded < f64::from(i32::MIN) || rounded > f64::from(i32::MAX) {
        return Err(ComputerError::InvalidArguments(
            "coordinate remap is out of range".into(),
        ));
    }
    Ok(rounded as i32)
}

fn run_batch(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    actions: Vec<Value>,
    stop_on_error: bool,
) -> Response {
    // Steps share one hold. The drop releases only when this is the outermost session.
    let _session = InputSession::open(backend);
    let mut results = Vec::with_capacity(actions.len());
    let mut first_error: Option<(usize, ErrorBody)> = None;
    for (index, step) in actions.into_iter().enumerate() {
        let response = dispatch_batch_step(backend, context, step);
        let failed = !response.ok;
        if failed && first_error.is_none() {
            if let Some(error) = response.error.clone() {
                first_error = Some((index, error));
            }
        }
        results.push(response.to_value());
        if failed && stop_on_error {
            return attach_post_state(
                backend,
                context,
                PostKind::Mutating,
                batch_response(results, Some(index), first_error),
            );
        }
    }
    attach_post_state(
        backend,
        context,
        PostKind::Mutating,
        batch_response(results, None, first_error),
    )
}

fn dispatch_batch_step(
    backend: &dyn DesktopBackend,
    outer: &CallContext,
    value: Value,
) -> Response {
    match prepare_batch_step(outer, value) {
        Ok((action, context)) => dispatch_with_context(backend, action, &context),
        Err(error) => Response::from_error(error),
    }
}

fn prepare_batch_step(
    outer: &CallContext,
    mut value: Value,
) -> Result<(Action, CallContext), ComputerError> {
    let Some(object) = value.as_object_mut() else {
        return Err(ComputerError::InvalidArguments(
            "batch step must be a JSON object".into(),
        ));
    };
    if object.get("action").and_then(Value::as_str) == Some("batch") {
        return Err(ComputerError::InvalidArguments(
            "nested batch is not allowed".into(),
        ));
    }
    let mut context = outer.clone();
    overlay_context(&mut context, object)?;
    context.return_state = false;
    context.state_diff = false;
    context.include_screenshot = false;
    let action = serde_json::from_value(value)
        .map_err(|error| ComputerError::InvalidArguments(error.to_string()))?;
    Ok((action, context))
}

fn overlay_context(
    context: &mut CallContext,
    object: &mut Map<String, Value>,
) -> Result<(), ComputerError> {
    if object.contains_key("app") {
        context.app = take_stringish(object, "app")?;
    }
    if object.contains_key("window") {
        context.window = take_stringish(object, "window")?;
    }
    if object.contains_key("element") && !element_is_a_condition(object) {
        context.element = take_optional_u32(object, "element")?;
    }
    if object.contains_key("diff") {
        context.diff = take_bool(object, "diff")?;
    }
    if object.contains_key("full") {
        context.full = take_bool(object, "full")?;
    }
    let _ = object.remove("return_state");
    let _ = object.remove("state_diff");
    let _ = object.remove("include_screenshot");
    let _ = object.remove("id");
    let _ = object.remove("stop_on_error");
    if object.contains_key("coord") {
        context.coord = take_stringish(object, "coord")?;
    }
    if object.contains_key("session") {
        context.session = take_stringish(object, "session")?;
    }
    if object.contains_key("tier") {
        context.tier = take_stringish(object, "tier")?;
    }
    if object.contains_key("adapter") {
        context.adapter = take_stringish(object, "adapter")?;
    }
    if object.contains_key("display") {
        context.display = take_stringish(object, "display")?;
    }
    if object.contains_key("require_visible") {
        context.require_visible = take_bool(object, "require_visible")?;
    }
    if object.contains_key("allow_foreground") {
        context.allow_foreground = take_bool(object, "allow_foreground")?;
    }
    if object.contains_key("require_background") {
        context.require_background = take_bool(object, "require_background")?;
    }
    if object.contains_key("no_overlay") {
        context.no_overlay = take_bool(object, "no_overlay")?;
    }
    Ok(())
}

fn batch_response(
    results: Vec<Value>,
    stopped_at: Option<usize>,
    first_error: Option<(usize, ErrorBody)>,
) -> Response {
    let mut fields = Map::new();
    fields.insert("action".into(), json!("batch"));
    fields.insert("results".into(), Value::Array(results));
    if let Some(index) = stopped_at {
        fields.insert("stopped_at".into(), json!(index));
    }
    match first_error {
        Some((index, error)) => {
            let mut response = Response::failure(
                error.code,
                format!("batch failed at action {index}: {}", error.message),
            );
            response.fields = fields;
            response
        }
        None => Response {
            ok: true,
            id: None,
            error: None,
            fields,
        },
    }
}

fn attach_post_state(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    kind: PostKind,
    mut response: Response,
) -> Response {
    // `state_diff` implies the post-state: it is the same attachment without
    // the full tree.
    if !(context.return_state || context.state_diff) || matches!(kind, PostKind::ReadOnly) {
        return response;
    }
    let mut warnings = Vec::new();
    let mut state = match backend.tree(&tree_query(context)) {
        Ok(report) => {
            // `diff` is `None` when this observation created the baseline.
            let baseline = if report.diff.is_some() {
                "existing"
            } else {
                "created"
            };
            let mut state = json!({
                "app": report.app,
                "pid": report.pid,
                "window_id": report.window_id,
                "revision": report.revision,
                "diff": report.diff,
                "truncated": report.truncated,
                "baseline": baseline,
            });
            // `--state-diff` keeps the delta and drops the full tree. The full
            // tree is the expensive half of this payload.
            if !context.state_diff {
                state["tree"] = json!(report.full_text);
            }
            state
        }
        Err(error) => {
            if !response.ok {
                return response;
            }
            warnings.push(format!("tree snapshot failed: {error}"));
            json!({
                "app": null,
                "pid": null,
                "window_id": null,
                "revision": null,
                "tree": null,
                "diff": null,
                "truncated": null,
                "baseline": null,
            })
        }
    };
    if context.include_screenshot {
        let shot = if response.ok && matches!(kind, PostKind::Screenshot) {
            Ok(Value::Object(response.fields.clone()))
        } else if matches!(kind, PostKind::Screenshot) {
            Err(ComputerError::ExecutionFailed(
                response
                    .error
                    .as_ref()
                    .map(|error| error.message.clone())
                    .unwrap_or_else(|| "screenshot failed".into()),
            ))
        } else {
            screenshot(
                backend,
                context,
                ScreenshotArgs {
                    output: None,
                    region: None,
                    format: None,
                    max_width: None,
                    canvas: None,
                    include_base64: false,
                    window_crop: false,
                    marks: false,
                },
            )
        };
        match shot {
            Ok(value) => state["screenshot"] = value,
            Err(error) => {
                state["screenshot"] = Value::Null;
                warnings.push(format!("screenshot failed: {error}"));
            }
        }
    }
    response.fields.insert("state".into(), state);
    push_warnings(&mut response, warnings);
    response
}

fn push_warnings(response: &mut Response, extra: Vec<String>) {
    if extra.is_empty() {
        return;
    }
    let mut warnings = match response.fields.remove("warnings") {
        Some(Value::Array(items)) => items,
        Some(other) => vec![other],
        None => Vec::new(),
    };
    for warning in extra {
        warnings.push(json!(warning));
    }
    response
        .fields
        .insert("warnings".into(), Value::Array(warnings));
}

fn cursor_position(backend: &dyn DesktopBackend) -> Result<Value, ComputerError> {
    let (x, y) = backend.cursor_position()?;
    Ok(json!({"action": "cursor_position", "x": x, "y": y}))
}

fn invalid_arguments(message: &str) -> ComputerError {
    ComputerError::InvalidArguments(message.to_string())
}

fn unsupported_error(operation: &str) -> ComputerError {
    ComputerError::Unsupported(format!(
        "{operation} is not available on {}",
        std::env::consts::OS
    ))
}

fn menu_list(backend: &dyn DesktopBackend, app: &str, depth: u32) -> Result<Value, ComputerError> {
    let report = backend.menu_list(app, depth)?;
    Ok(json!({
        "action": "menu_list",
        "app": report.app,
        "sections": report.sections,
    }))
}

fn menu_click(backend: &dyn DesktopBackend, app: &str, path: &str) -> Result<Value, ComputerError> {
    let parsed = crate::menu_path::parse(path)?;
    let result = backend.menu_click(app, &parsed)?;
    Ok(json!({
        "action": "menu_click",
        "app": app,
        "path": parsed.segments,
        "pressed": result.pressed,
    }))
}

fn apple_event_action(
    backend: &dyn DesktopBackend,
    app: &str,
    script: &str,
    language: AppleEventLanguage,
) -> Result<Value, ComputerError> {
    let report = backend.apple_event(app, script, language)?;
    Ok(json!({
        "action": "apple_event",
        "app": app,
        "language": language.as_str(),
        "script_bytes": report.script_bytes,
        "script_hash": report.script_hash,
        "stdout": report.stdout,
        "stderr": report.stderr,
        "exit_status": report.exit_status,
    }))
}

fn shortcuts_run_action(name: String) -> Result<Value, ComputerError> {
    let report = backend_shortcuts_run(&name)?;
    Ok(json!({
        "action": "shortcuts_run",
        "name": name,
        "exit_status": report.exit_status,
        "stderr": report.stderr,
    }))
}

fn dock_list_action(backend: &dyn DesktopBackend) -> Result<Value, ComputerError> {
    let report = backend.dock_list()?;
    Ok(json!({
        "action": "dock_list",
        "items": report.items,
    }))
}

fn dock_click_action(backend: &dyn DesktopBackend, title: &str) -> Result<Value, ComputerError> {
    let report = backend.dock_click(title)?;
    Ok(json!({
        "action": "dock_click",
        "title": title,
        "pressed": report.pressed,
    }))
}

fn spaces_action(backend: &dyn DesktopBackend) -> Result<Value, ComputerError> {
    let report = backend.spaces()?;
    Ok(json!({
        "action": "spaces",
        "screens": report.screens,
        "note": report.note,
    }))
}

/// `spaces move` is deliberately not implemented. Moving a window between
/// managed spaces needs the private SkyLight window-to-space symbols, which
/// this build does not call. A built-in `unsupported` is safer than an
/// unverified private call that could destabilize the session.
fn spaces_move_action(to_space: u32) -> Result<Value, ComputerError> {
    let reason = if crate::tier::skylight_compiled() {
        "the private SkyLight window-to-space API is not implemented"
    } else {
        "this build has no skylight feature and the private SkyLight window-to-space API is not implemented"
    };
    Err(ComputerError::Unsupported(format!(
        "spaces move --to-space {to_space}: {reason}; use window move --to-display instead"
    )))
}

// `shortcuts` does not need the desktop backend, but the helper signature
// mirrors the others and leaves room for backend-specific policy hooks.
fn backend_shortcuts_run(name: &str) -> Result<crate::shortcuts::ShortcutsReport, ComputerError> {
    crate::shortcuts::run(name)
}

fn session_prune(dry_run: bool, ttl_secs: Option<u64>) -> Result<Value, ComputerError> {
    let ttl = match ttl_secs {
        Some(seconds) => Duration::from_secs(seconds),
        None => cache::DEFAULT_SESSION_TTL,
    };
    let report = cache::prune_sessions(
        &cache::sessions_root(),
        &cache::PruneOptions {
            dry_run,
            ttl,
            now: SystemTime::now(),
        },
        &cache::pid_alive,
    )?;
    Ok(json!({
        "action": "session_prune",
        "dry_run": dry_run,
        "ttl_secs": ttl.as_secs(),
        "removed": report.removed,
        "kept": report.kept,
    }))
}

struct ScreenshotArgs {
    output: Option<String>,
    region: Option<Region>,
    format: Option<ImageFormat>,
    max_width: Option<u32>,
    canvas: Option<String>,
    include_base64: bool,
    /// When set, a window target crops `region` in window-local points.
    /// Screenshot still refuses a region together with a window.
    window_crop: bool,
    /// Stamp numbered element rectangles on the image before encoding.
    marks: bool,
}

fn screenshot(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    args: ScreenshotArgs,
) -> Result<Value, ComputerError> {
    let ScreenshotArgs {
        output,
        region,
        format,
        max_width,
        canvas,
        include_base64,
        window_crop,
        marks,
    } = args;
    let targeted = context.app.is_some() || context.window.is_some();
    if region.is_some() && targeted && !window_crop {
        return Err(ComputerError::InvalidArguments(
            "pass a region or a window target, not both".into(),
        ));
    }
    let format = format.unwrap_or_default();
    let max_width = max_width.unwrap_or(image_util::DEFAULT_SCREENSHOT_MAX_WIDTH);
    let canvas = canvas
        .as_deref()
        .map(image_util::parse_canvas)
        .transpose()?;
    let raw = if targeted {
        if let Some(error) = backend.display_block() {
            return Err(error);
        }
        let target = resolve_screenshot_window(backend, context)?;
        // A window capture cannot be redirected onto another monitor, so a
        // filtered request only works when the window already sits there.
        if let Some(filter) = display_filter(backend, context)? {
            if !crate::isolation::window_inside(&filter, target.bounds) {
                return Err(ComputerError::InvalidArguments(format!(
                    "window {}x{}+{}+{} is not inside display {} ({} covers {})",
                    target.bounds.width,
                    target.bounds.height,
                    target.bounds.x,
                    target.bounds.y,
                    filter.display.id,
                    filter.display.name,
                    filter.bounds()
                )));
            }
        }
        let captured = backend.capture_window(&target)?;
        match region {
            Some(region) if window_crop => crop_window_region(captured, region)?,
            _ => captured,
        }
    } else {
        // Capture the chosen monitor only. `capture_region` maps the exact
        // monitor rectangle onto one monitor capture, so the screenshot map
        // origin is that monitor's origin and the image is its pixels.
        let region = match display_filter(backend, context)? {
            Some(filter) => Some(filter.region),
            None => region,
        };
        backend.capture_screen(region)?
    };
    let prepared = image_util::prepare_shot(
        raw.frame.image(),
        image_util::ShotFit {
            scale: raw.frame.scale(),
            origin_x: raw.origin_x,
            origin_y: raw.origin_y,
            logical_width: raw.logical_width,
            logical_height: raw.logical_height,
            max_width,
            canvas,
        },
    )?;
    let mut image = prepared.image;
    let mut warnings = raw.warnings;
    if marks {
        let applied = apply_marks(&mut image, &prepared.map, context, backend)?;
        if applied == 0 {
            // A silent zero is indistinguishable from "the marks were drawn".
            // Say so instead of writing a clean capture that answers `ok`.
            warnings.push("marks: no accessibility element frame fell inside this capture".into());
        }
    }
    let encoded = image_util::encode_rgba(&image, format)?;
    let path = match output {
        Some(path) => PathBuf::from(path),
        None => default_screenshot_path(format),
    };
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::write(&path, &encoded.bytes)?;
    if let Err(error) = cache::store_screenshot_map(context.session.as_deref(), &prepared.map) {
        warnings.push(format!("screenshot map was not cached: {error}"));
    }
    let mut body = serde_json::to_value(&prepared.map).map_err(|error| {
        ComputerError::ExecutionFailed(format!("screenshot map encode failed: {error}"))
    })?;
    body["path"] = json!(path_string(&path));
    body["format"] = json!(format.as_str());
    body["capture_source"] = json!(raw.capture_source);
    if raw.occluded {
        body["occluded"] = json!(true);
    }
    if !warnings.is_empty() {
        body["warnings"] = json!(warnings);
    }
    if include_base64 {
        body["data"] = json!(encode_base64(&encoded.bytes));
    }
    Ok(body)
}

fn resolve_screenshot_window(
    backend: &dyn DesktopBackend,
    context: &CallContext,
) -> Result<WindowTarget, ComputerError> {
    let listed = backend.list_windows()?;
    if let Some(app) = context.app.as_deref() {
        let apps = backend.list_apps(true)?;
        let selector = parse_app_selector(app)?;
        let app = resolve_app(&apps, &selector)?;
        let owned = filter_windows_for_pid(&listed.windows, app.pid);
        let selector = match context.window.as_deref() {
            Some(window) => parse_window_selector(window)?,
            None => WindowSelector::Index(0),
        };
        let window = resolve_window(&owned, &selector)?;
        return Ok(window_target(window));
    }
    let selector = parse_window_selector(context.window.as_deref().unwrap_or(""))?;
    let window = resolve_window(&listed.windows, &selector)?;
    Ok(window_target(window))
}

fn window_target(window: &crate::target::WindowInfo) -> WindowTarget {
    WindowTarget {
        window_id: window.id,
        pid: window.pid,
        bounds: window.bounds,
    }
}

/// Crop `region` in window-local logical points. The far edge may equal the window size.
fn crop_window_region(
    raw: crate::types::RawCapture,
    region: Region,
) -> Result<crate::types::RawCapture, ComputerError> {
    region_inside(region, raw.logical_width, raw.logical_height)?;
    let scale = raw.frame.scale();
    let x = logical_to_device(i64::from(region.x), scale)?;
    let y = logical_to_device(i64::from(region.y), scale)?;
    let width = logical_len_to_device(region.width, scale)?;
    let height = logical_len_to_device(region.height, scale)?;
    let image = raw.frame.image();
    if x >= image.width() || y >= image.height() {
        return Err(ComputerError::InvalidArguments(
            "region is outside the capture".into(),
        ));
    }
    let width = width.min(image.width() - x).max(1);
    let height = height.min(image.height() - y).max(1);
    let cropped = image::imageops::crop_imm(image, x, y, width, height).to_image();
    Ok(crate::types::RawCapture {
        frame: Frame::from_image(cropped, scale),
        origin_x: raw.origin_x.saturating_add(region.x),
        origin_y: raw.origin_y.saturating_add(region.y),
        logical_width: region.width,
        logical_height: region.height,
        capture_source: raw.capture_source,
        occluded: raw.occluded,
        warnings: raw.warnings,
    })
}

fn logical_to_device(value: i64, scale: f64) -> Result<u32, ComputerError> {
    if value < 0 {
        return Err(ComputerError::InvalidArguments(
            "region is outside the capture".into(),
        ));
    }
    let pixels = (value as f64) * scale;
    if !pixels.is_finite() || pixels < 0.0 || pixels > f64::from(u32::MAX) {
        return Err(ComputerError::ExecutionFailed(
            "region pixel coordinate is out of range".into(),
        ));
    }
    Ok(pixels.round() as u32)
}

fn logical_len_to_device(value: u32, scale: f64) -> Result<u32, ComputerError> {
    let pixels = logical_to_device(i64::from(value), scale)?;
    Ok(pixels.max(1))
}

fn default_screenshot_path(format: ImageFormat) -> PathBuf {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = format!(
        "computer-use-{}-{nanos}-{sequence}.{}",
        std::process::id(),
        format.extension()
    );
    std::env::temp_dir().join(name)
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn click(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    x: Option<i32>,
    y: Option<i32>,
    button: MouseButton,
    double: bool,
    count: u8,
) -> Response {
    let count = match effective_click_count(double, count) {
        Ok(count) => count,
        Err(error) => return Response::from_error(error),
    };
    match (context.element, x, y) {
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
            Response::failure("invalid_arguments", "pass element or x and y, not both")
        }
        (None, None, None) => Response::failure(
            "invalid_arguments",
            "click requires element, or both x and y",
        ),
        (None, Some(_), None) | (None, None, Some(_)) => {
            Response::failure("invalid_arguments", "x and y must be passed together")
        }
        (Some(_index), None, None) => {
            element_command(backend, context, ElementOp::Press { count, button })
        }
        (None, Some(x), Some(y)) => coordinate_click(backend, context, x, y, button, count),
    }
}

fn effective_click_count(double: bool, count: u8) -> Result<u8, ComputerError> {
    if count == 0 || count > 3 {
        return Err(ComputerError::InvalidArguments(
            "click count must be 1, 2, or 3".into(),
        ));
    }
    if double && count != 1 && count != 2 {
        return Err(ComputerError::InvalidArguments(
            "double and count disagree".into(),
        ));
    }
    if double {
        Ok(2)
    } else {
        Ok(count)
    }
}

fn coordinate_click(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    x: i32,
    y: i32,
    button: MouseButton,
    count: u8,
) -> Response {
    let kind = InputKind::Click {
        x,
        y,
        button,
        count,
    };
    // Tier rejection, including synthetic without the feature, happens before hit-testing.
    if let Err(error) = planned_tiers(backend, context) {
        return Response::from_error(error);
    }
    let mut warnings = Vec::new();
    let hit = match backend.hit_test(x, y) {
        Ok(hit) => hit,
        Err(error) => {
            warnings.push(format!("hit test failed: {error}"));
            None
        }
    };
    if let Some(title) = hit.as_ref().and_then(|hit| hit.title.as_deref()) {
        if policy::sensitive_title(title) {
            if let Err(error) = confirm_title(context, "click", "sensitive control title") {
                return Response::from_error(error);
            }
        }
    }
    match run_attempts(backend, context, kind) {
        Ok(report) => {
            let action_name = match count {
                2 => "double_click",
                3 => "triple_click",
                _ => "click",
            };
            let body = json!({
                "action": action_name,
                "x": x,
                "y": y,
                "button": button.as_str(),
                "double": count == 2,
                "count": count,
                "hit": hit_value(hit),
            });
            Response::success(finish_input(body, report, warnings))
        }
        Err(error) => Response::from_error(error),
    }
}

fn hit_value(hit: Option<crate::element::HitInfo>) -> Value {
    match hit {
        Some(hit) => json!({"role": hit.role, "title": hit.title}),
        None => Value::Null,
    }
}

fn type_command(backend: &dyn DesktopBackend, context: &CallContext, text: String) -> Response {
    if context.element.is_some() {
        return element_command(backend, context, ElementOp::Type { text });
    }
    if text.is_empty() {
        return Response::from_error(ComputerError::InvalidArguments(
            "text must not be empty".into(),
        ));
    }
    if text.contains('\0') {
        return Response::from_error(ComputerError::InvalidArguments(
            "text must not contain a null byte".into(),
        ));
    }
    let chars = text.chars().count();
    match run_attempts(backend, context, InputKind::Type { text: text.clone() }) {
        Ok(report) => {
            let body = json!({
                "action": "type",
                "text": text,
                "chars": chars,
            });
            Response::success(finish_input(body, report, Vec::new()))
        }
        Err(error) => Response::from_error(error),
    }
}

fn element_command(backend: &dyn DesktopBackend, context: &CallContext, op: ElementOp) -> Response {
    if let Err(error) = precheck(&op) {
        return Response::from_error(error);
    }
    if let Err(error) = confirm_element_title(backend, context, &op) {
        return Response::from_error(error);
    }
    let index = match context.element {
        Some(index) => index,
        None => {
            return Response::from_error(ComputerError::InvalidArguments(
                "element is required".into(),
            ))
        }
    };
    let request = ElementRequest {
        op,
        index,
        require_visible: context.require_visible,
        app: context.app.clone(),
        window: context.window.clone(),
        session: context.session.clone(),
        tier: context.tier.clone(),
        allow_foreground: context.allow_foreground,
        require_background: context.require_background,
        no_overlay: context.no_overlay,
    };
    match backend.element_action(&request) {
        Ok(outcome) => Response::success(outcome_value(&outcome)),
        Err(error) => element_error_response(error),
    }
}

fn confirm_element_title(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    op: &ElementOp,
) -> Result<(), ComputerError> {
    let press = matches!(op, ElementOp::Press { .. })
        || matches!(op, ElementOp::Perform { name } if name.eq_ignore_ascii_case("AXPress"));
    if !press {
        return Ok(());
    }
    let Some(index) = context.element else {
        return Ok(());
    };
    let Ok(Some(peek)) = backend.peek_element(
        context.app.as_deref(),
        context.window.as_deref(),
        context.session.as_deref(),
        index,
    ) else {
        return Ok(());
    };
    if peek.title.as_deref().is_some_and(policy::sensitive_title) {
        confirm_title(context, "press", "sensitive control title")?;
    }
    Ok(())
}

fn confirm_title(context: &CallContext, action: &str, reason: &str) -> Result<(), ComputerError> {
    let loaded = policy::load_policy(context.policy.as_deref().map(Path::new))?;
    let command = policy::confirm_program(&loaded, context.confirm_cmd.as_deref());
    policy::run_confirm(
        command.as_deref(),
        loaded.timeout_secs(),
        &policy::ConfirmDocument {
            action,
            bundle_id: None,
            path: None,
            pid: None,
            window_id: None,
            element: context.element,
            reason,
            extra: None,
        },
    )
}

fn precheck(op: &ElementOp) -> Result<(), ComputerError> {
    match op {
        ElementOp::Press { count, .. } => element::validate_count(*count),
        ElementOp::SetValue { value } => element::validate_text(value, true),
        ElementOp::Type { text } => element::validate_text(text, false),
        ElementOp::SelectText { start, end } => element::validate_range(*start, *end, None),
        ElementOp::Perform { name } => element::validate_perform_name(name),
        ElementOp::Focus | ElementOp::ScrollTo => Ok(()),
    }
}

fn outcome_value(outcome: &ElementOutcome) -> Value {
    let mut body = json!({
        "action": outcome.action,
        "element": outcome.element,
        "revision": outcome.revision,
        "tier": outcome.tier,
        "cursor_moved": outcome.cursor_moved,
    });
    if let Some(restored) = outcome.focus_restored {
        body["focus_restored"] = json!(restored);
    }
    if !outcome.warnings.is_empty() {
        body["warnings"] = json!(outcome.warnings);
    }
    if let Some(via) = &outcome.via {
        body["via"] = json!(via);
    }
    if let Some(name) = &outcome.name {
        body["name"] = json!(name);
    }
    if let Some(count) = outcome.count {
        body["count"] = json!(count);
    }
    body
}

fn element_error_response(error: ElementError) -> Response {
    let mut response = Response::from_error(error.error);
    if let Some(tree) = error.tree {
        response.fields.insert("tree".into(), json!(tree));
    }
    response
}

fn move_mouse(backend: &dyn DesktopBackend, context: &CallContext, x: i32, y: i32) -> Response {
    match run_attempts(backend, context, InputKind::Move { x, y }) {
        Ok(report) => Response::success(finish_input(
            json!({"action": "move", "x": x, "y": y}),
            report,
            Vec::new(),
        )),
        Err(error) => Response::from_error(error),
    }
}

fn press_key(backend: &dyn DesktopBackend, context: &CallContext, keys: &str) -> Response {
    let strokes = match key_strokes(keys) {
        Ok(strokes) => strokes,
        Err(error) => return Response::from_error(error),
    };
    let mut body = json!({
        "action": "key",
        "keys": keys,
    });
    if strokes.len() == 1 {
        let modifiers: Vec<&str> = strokes[0]
            .modifiers
            .iter()
            .copied()
            .map(Modifier::as_str)
            .collect();
        body["modifiers"] = json!(modifiers);
        body["key"] = json!(strokes[0].key);
    } else {
        body["strokes"] = json!(strokes.len());
    }
    match run_attempts(backend, context, InputKind::Key { strokes }) {
        Ok(report) => Response::success(finish_input(body, report, Vec::new())),
        Err(error) => Response::from_error(error),
    }
}

fn key_strokes(keys: &str) -> Result<Vec<KeyStroke>, ComputerError> {
    let combos = parse_key_sequence(keys)?;
    let mut strokes = Vec::with_capacity(combos.len());
    for combo in combos {
        validate_key(&combo.key)?;
        strokes.push(KeyStroke {
            modifiers: combo.modifiers,
            key: combo.key,
        });
    }
    Ok(strokes)
}

fn drag(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    path: Vec<Point>,
    button: MouseButton,
) -> Response {
    if path.len() < 2 {
        return Response::from_error(ComputerError::InvalidArguments(
            "drag needs at least 2 points".into(),
        ));
    }
    let points: Vec<(i32, i32)> = path.iter().map(|point| (point.x, point.y)).collect();
    let moves = points.len() - 1;
    match run_attempts(backend, context, InputKind::Drag { points, button }) {
        Ok(report) => Response::success(finish_input(
            json!({
                "action": "drag",
                "button": button.as_str(),
                "points": path.len(),
                "moves": moves,
            }),
            report,
            Vec::new(),
        )),
        Err(error) => Response::from_error(error),
    }
}

fn pointer_half(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    x: i32,
    y: i32,
    button: MouseButton,
    down: bool,
) -> Response {
    let kind = if down {
        InputKind::MouseDown { x, y, button }
    } else {
        InputKind::MouseUp { x, y, button }
    };
    let name = if down { "mouse_down" } else { "mouse_up" };
    match run_attempts(backend, context, kind) {
        Ok(report) => Response::success(finish_input(
            json!({
                "action": name,
                "x": x,
                "y": y,
                "button": button.as_str(),
            }),
            report,
            Vec::new(),
        )),
        Err(error) => Response::from_error(error),
    }
}

fn hold_key(backend: &dyn DesktopBackend, context: &CallContext, keys: &str, ms: u64) -> Response {
    let stroke = match one_hold(keys, ms) {
        Ok(stroke) => stroke,
        Err(error) => return Response::from_error(error),
    };
    match run_attempts(
        backend,
        context,
        InputKind::HoldKey {
            modifiers: stroke.modifiers.clone(),
            key: stroke.key.clone(),
            ms,
        },
    ) {
        Ok(report) => Response::success(finish_input(
            json!({
                "action": "hold_key",
                "keys": keys,
                "key": stroke.key,
                "ms": ms,
            }),
            report,
            Vec::new(),
        )),
        Err(error) => Response::from_error(error),
    }
}

fn one_hold(keys: &str, ms: u64) -> Result<KeyStroke, ComputerError> {
    if ms > tier::MAX_HOLD_MS {
        return Err(ComputerError::InvalidArguments(format!(
            "hold_key ms must be at most {}",
            tier::MAX_HOLD_MS
        )));
    }
    if keys.split_whitespace().count() != 1 {
        return Err(ComputerError::InvalidArguments(
            "hold_key accepts one key combination".into(),
        ));
    }
    let combo = parse_key_combo(keys)?;
    validate_key(&combo.key)?;
    Ok(KeyStroke {
        modifiers: combo.modifiers,
        key: combo.key,
    })
}

fn clipboard_chars(action: &Action) -> Option<u64> {
    match action {
        Action::ClipboardSet {
            text: Some(text), ..
        } => Some(u64::try_from(text.chars().count()).unwrap_or(u64::MAX)),
        Action::BrowserType { text, .. } => {
            Some(u64::try_from(text.chars().count()).unwrap_or(u64::MAX))
        }
        _ => None,
    }
}

fn preflight_action(action: &Action) -> Result<(), ComputerError> {
    match action {
        Action::Click { double, count, .. } => {
            effective_click_count(*double, *count)?;
        }
        Action::Press { count, .. } => element::validate_count(*count)?,
        Action::Drag { path, .. } if path.len() < 2 => {
            return Err(ComputerError::InvalidArguments(
                "drag needs at least 2 points".into(),
            ));
        }
        Action::HoldKey { keys, ms } => {
            one_hold(keys, *ms)?;
        }
        Action::Key { keys } => {
            key_strokes(keys)?;
        }
        Action::ClipboardSet { text, image } => match (text, image) {
            (Some(text), None) if text.is_empty() => {
                return Err(ComputerError::InvalidArguments(
                    "clipboard text must not be empty".into(),
                ));
            }
            (None, None) => {
                return Err(ComputerError::InvalidArguments(
                    "clipboard set requires text or image".into(),
                ));
            }
            (Some(_), Some(_)) => {
                return Err(ComputerError::InvalidArguments(
                    "pass clipboard text or image, not both".into(),
                ));
            }
            _ => {}
        },
        _ => {}
    }
    Ok(())
}

fn clipboard_get(
    backend: &dyn DesktopBackend,
    context: &CallContext,
) -> Result<serde_json::Value, ComputerError> {
    match backend.clipboard_get()? {
        ClipboardContents::Empty => Ok(json!({
            "action": "clipboard_get",
            "kind": "empty",
        })),
        ClipboardContents::Text(text) => {
            let chars = text.chars().count();
            Ok(json!({
                "action": "clipboard_get",
                "kind": "text",
                "text": text,
                "chars": chars,
            }))
        }
        ClipboardContents::Image {
            width,
            height,
            rgba,
        } => {
            let frame = Frame::new(width, height, 1.0, rgba)?;
            let encoded = image_util::encode_rgba(frame.image(), ImageFormat::Png)?;
            let dir = cache::session_dir(context.session.as_deref())?;
            let path = dir.join("clipboard.png");
            cache::write_secret_bytes(&path, &encoded.bytes)?;
            Ok(json!({
                "action": "clipboard_get",
                "kind": "image",
                "path": path_string(&path),
                "width": width,
                "height": height,
            }))
        }
    }
}

fn clipboard_set(
    backend: &dyn DesktopBackend,
    text: Option<String>,
    image: Option<String>,
) -> Result<serde_json::Value, ComputerError> {
    match (text, image) {
        (Some(text), None) => {
            let chars = text.chars().count();
            backend.clipboard_set_text(&text)?;
            Ok(json!({
                "action": "clipboard_set",
                "kind": "text",
                "chars": chars,
            }))
        }
        (None, Some(path)) => {
            let (width, height, rgba) = decode_clipboard_image(&path)?;
            backend.clipboard_set_image(width, height, &rgba)?;
            Ok(json!({
                "action": "clipboard_set",
                "kind": "image",
                "width": width,
                "height": height,
            }))
        }
        _ => Err(ComputerError::InvalidArguments(
            "clipboard set requires text or image".into(),
        )),
    }
}

fn decode_clipboard_image(path: &str) -> Result<(u32, u32, Vec<u8>), ComputerError> {
    let image = image::ImageReader::open(path)
        .map_err(|error| {
            ComputerError::InvalidArguments(format!("clipboard image could not be opened: {error}"))
        })?
        .decode()
        .map_err(|error| {
            ComputerError::InvalidArguments(format!(
                "clipboard image could not be decoded: {error}"
            ))
        })?
        .to_rgba8();
    let width = image.width();
    let height = image.height();
    Ok((width, height, image.into_raw()))
}

fn scroll(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    point: (Option<i32>, Option<i32>),
    delta: (Option<i32>, Option<i32>),
    direction: Option<ScrollDirection>,
    amount: Option<i32>,
) -> Response {
    let (x, y) = point;
    let (dx, dy) = delta;
    if x.is_some() != y.is_some() {
        return Response::from_error(ComputerError::InvalidArguments(
            "scroll x and y must be set together".into(),
        ));
    }
    let (scroll_dx, scroll_dy) = match resolve_scroll(dx, dy, direction, amount) {
        Ok(delta) => delta,
        Err(error) => return Response::from_error(error),
    };
    match run_attempts(
        backend,
        context,
        InputKind::Scroll {
            x,
            y,
            dx: scroll_dx,
            dy: scroll_dy,
        },
    ) {
        Ok(report) => {
            let body = json!({
                "action": "scroll",
                "x": x,
                "y": y,
                "dx": scroll_dx,
                "dy": scroll_dy,
            });
            Response::success(finish_input(scroll_body(body, &report), report, Vec::new()))
        }
        Err(error) => Response::from_error(error),
    }
}

/// Add the verified-scroll fields when the backend reported them.
///
/// `via` is `event` when the posted wheel event moved the scroll bar and
/// `ax_scrollbar` when `AXValue` had to be written instead. `step` is the
/// per-line fraction (0..=1) the AX scroll-bar path computed from the
/// content-over-visible sizes, surfaced for transparency even when the wheel
/// event tier did the scrolling. Windows and Linux report no outcome, so
/// their response keeps its existing shape.
fn scroll_body(mut body: Value, report: &crate::tier::InputReport) -> Value {
    let Some(outcome) = &report.scroll else {
        return body;
    };
    body["via"] = json!(outcome.via.as_str());
    body["scrolled"] = json!(outcome.scrolled);
    body["before"] = json!(outcome.before);
    body["after"] = json!(outcome.after);
    body["step"] = json!(outcome.step);
    body
}

fn planned_tiers(
    backend: &dyn DesktopBackend,
    context: &CallContext,
) -> Result<Vec<TierName>, ComputerError> {
    let requested = crate::tier::parse_tier(context.tier.as_deref())?;
    crate::tier::plan(
        requested,
        crate::tier::InputClass::Background,
        backend.input_support(),
        crate::tier::skylight_compiled(),
    )
}

fn run_attempts(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    kind: InputKind,
) -> Result<crate::tier::InputReport, ComputerError> {
    attempt::run(backend, &attempt_flags(context), kind)
}

fn attempt_flags(context: &CallContext) -> AttemptFlags {
    AttemptFlags {
        app: context.app.clone(),
        window: context.window.clone(),
        tier: context.tier.clone(),
        allow_foreground: context.allow_foreground,
        require_background: context.require_background,
        no_overlay: context.no_overlay,
        session: context.session.clone(),
    }
}

fn finish_input(
    mut body: Value,
    report: crate::tier::InputReport,
    mut warnings: Vec<String>,
) -> Value {
    warnings.extend(report.warnings);
    body["tier"] = json!(report.tier);
    body["cursor_moved"] = json!(report.cursor_moved);
    if let Some(restored) = report.focus_restored {
        body["focus_restored"] = json!(restored);
    }
    if !warnings.is_empty() {
        body["warnings"] = json!(warnings);
    }
    body
}

pub(crate) fn resolve_scroll(
    dx: Option<i32>,
    dy: Option<i32>,
    direction: Option<ScrollDirection>,
    amount: Option<i32>,
) -> Result<(i32, i32), ComputerError> {
    let has_delta = dx.is_some() || dy.is_some();
    let has_direction = direction.is_some() || amount.is_some();
    if has_delta && has_direction {
        return Err(ComputerError::InvalidArguments(
            "use either dx/dy or direction/amount, not both".into(),
        ));
    }
    if let Some(direction) = direction {
        let amount = amount.unwrap_or(1);
        return Ok(match direction {
            ScrollDirection::Up => (0, -amount),
            ScrollDirection::Down => (0, amount),
            ScrollDirection::Left => (-amount, 0),
            ScrollDirection::Right => (amount, 0),
        });
    }
    if amount.is_some() {
        return Err(ComputerError::InvalidArguments(
            "amount requires direction".into(),
        ));
    }
    if !has_delta {
        return Err(ComputerError::InvalidArguments(
            "scroll requires dx/dy or direction".into(),
        ));
    }
    Ok((dx.unwrap_or(0), dy.unwrap_or(0)))
}

struct WaitArgs<'a> {
    ms: Option<u64>,
    seconds: Option<f64>,
    element: Option<u32>,
    element_state: Option<&'a str>,
    text: Option<&'a str>,
    value: Option<&'a str>,
    within_element: Option<u32>,
    window_state: Option<&'a str>,
    tree_change: bool,
    app_running: Option<&'a str>,
    title: Option<&'a str>,
    ignore_case: bool,
    timeout_ms: Option<u64>,
    poll_ms: Option<u64>,
}

fn wait_args(action: &Action) -> WaitArgs<'_> {
    match action {
        Action::Wait {
            ms,
            seconds,
            element,
            element_state,
            text,
            value,
            within_element,
            window_state,
            tree_change,
            app_running,
            title,
            ignore_case,
            timeout_ms,
            poll_ms,
        } => WaitArgs {
            ms: *ms,
            seconds: *seconds,
            element: *element,
            element_state: element_state.as_deref(),
            text: text.as_deref(),
            value: value.as_deref(),
            within_element: *within_element,
            window_state: window_state.as_deref(),
            tree_change: *tree_change,
            app_running: app_running.as_deref(),
            title: title.as_deref(),
            ignore_case: *ignore_case,
            timeout_ms: *timeout_ms,
            poll_ms: *poll_ms,
        },
        Action::Verify {
            element,
            element_state,
            text,
            value,
            within_element,
            window_state,
            tree_change,
            app_running,
            title,
            ignore_case,
            timeout_ms,
            poll_ms,
        } => WaitArgs {
            ms: None,
            seconds: None,
            element: *element,
            element_state: element_state.as_deref(),
            text: text.as_deref(),
            value: value.as_deref(),
            within_element: *within_element,
            window_state: window_state.as_deref(),
            tree_change: *tree_change,
            app_running: app_running.as_deref(),
            title: title.as_deref(),
            ignore_case: *ignore_case,
            timeout_ms: *timeout_ms,
            poll_ms: *poll_ms,
        },
        // `wait_action` is only reached for Wait and Verify.
        _ => unreachable!("wait_args is only called for wait and verify"),
    }
}

fn wait_spec(
    action: &Action,
    default_timeout_ms: u64,
) -> Result<Option<wait::WaitSpec>, ComputerError> {
    let args = wait_args(action);
    if args.element_state.is_some() && args.element.is_none() {
        return Err(ComputerError::InvalidArguments(
            "element-state requires --element".into(),
        ));
    }
    if args.within_element.is_some() && args.text.is_none() {
        return Err(ComputerError::InvalidArguments(
            "within-element requires --text".into(),
        ));
    }
    let mut conditions: Vec<wait::Condition> = Vec::new();
    // `--value` names the element it reads, so it does not add a second
    // element-state condition when both flags are present.
    if args.value.is_none() {
        if let Some(index) = args.element {
            let state = match args.element_state {
                Some(value) => wait::ElementState::parse(value)?,
                None => wait::ElementState::Exists,
            };
            conditions.push(wait::Condition::Element { index, state });
        }
    }
    if let Some(needle) = args.text {
        conditions.push(wait::Condition::Text {
            needle: needle.to_string(),
            ignore_case: args.ignore_case,
            within_element: args.within_element,
        });
    }
    if let Some(needle) = args.value {
        let index = args
            .element
            .ok_or_else(|| ComputerError::InvalidArguments("value requires --element".into()))?;
        conditions.push(wait::Condition::Value {
            index,
            needle: needle.to_string(),
            ignore_case: args.ignore_case,
        });
    }
    if let Some(value) = args.window_state {
        conditions.push(wait::Condition::Window {
            state: wait::WindowState::parse(value)?,
            selector: None,
        });
    }
    if args.tree_change {
        conditions.push(wait::Condition::TreeChange);
    }
    if let Some(name) = args.app_running {
        conditions.push(wait::Condition::AppRunning {
            name: name.to_string(),
            ignore_case: args.ignore_case,
        });
    }
    if let Some(needle) = args.title {
        conditions.push(wait::Condition::Title {
            needle: needle.to_string(),
            ignore_case: args.ignore_case,
        });
    }
    match conditions.len() {
        0 if args.ms.is_some() || args.seconds.is_some() => Ok(None),
        0 if matches!(action, Action::Verify { .. }) => Err(ComputerError::InvalidArguments(
            "verify needs a condition".into(),
        )),
        0 => Ok(None),
        1 if args.ms.is_some() || args.seconds.is_some() => Err(ComputerError::InvalidArguments(
            "wait takes a duration or a condition, not both".into(),
        )),
        1 => Ok(Some(wait::WaitSpec::new(
            conditions.pop().expect("one condition"),
            args.timeout_ms.unwrap_or(default_timeout_ms),
            args.poll_ms.unwrap_or(wait::DEFAULT_POLL_MS),
        )?)),
        _ => Err(ComputerError::InvalidArguments(
            "wait and verify accept exactly one condition".into(),
        )),
    }
}

fn wait_action(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    action: &Action,
) -> Result<Value, ComputerError> {
    let Some(spec) = wait_spec(action, wait::DEFAULT_TIMEOUT_MS)? else {
        let args = wait_args(action);
        let waited = resolve_wait(args.ms, args.seconds)?;
        thread::sleep(Duration::from_millis(waited));
        return Ok(json!({"waited_ms": waited}));
    };
    let started = Instant::now();
    let deadline = started + Duration::from_millis(spec.timeout_ms);
    let last = loop {
        let (matched, observed) = evaluate_condition(backend, context, &spec.condition)?;
        if matched {
            return Ok(json!({
                "waited_ms": started.elapsed().as_millis() as u64,
                "condition": spec.condition.as_str(),
                "observed": observed,
            }));
        }
        if Instant::now() >= deadline {
            break observed;
        }
        thread::sleep(Duration::from_millis(spec.poll_ms));
    };
    Err(ComputerError::Timeout(format!(
        "{} was not observed after {} ms; last observation: {last}",
        spec.condition.describe(),
        spec.timeout_ms
    )))
}

fn verify_response(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    action: &Action,
) -> Response {
    let spec = match wait_spec(action, wait::VERIFY_TIMEOUT_MS) {
        Ok(Some(spec)) => spec,
        Ok(None) => {
            return Response::from_error(ComputerError::InvalidArguments(
                "verify needs a condition".into(),
            ))
        }
        Err(error) => return Response::from_error(error),
    };
    let started = Instant::now();
    let deadline = started + Duration::from_millis(spec.timeout_ms);
    let last = loop {
        match evaluate_condition(backend, context, &spec.condition) {
            Ok((true, observed)) => {
                return Response::success(json!({
                    "verified": true,
                    "condition": spec.condition.as_str(),
                    "observed": observed,
                }))
            }
            Ok((false, observed)) => {
                if Instant::now() >= deadline {
                    break observed;
                }
            }
            Err(error) => return Response::from_error(error),
        }
        thread::sleep(Duration::from_millis(spec.poll_ms));
    };
    Response::from_error(ComputerError::VerificationFailed(format!(
        "{} was not true after {} ms",
        spec.condition.describe(),
        spec.timeout_ms
    )))
    .with_field("condition", json!(spec.condition.as_str()))
    .with_field("observed", last)
}

fn evaluate_condition(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    condition: &wait::Condition,
) -> Result<(bool, Value), ComputerError> {
    match condition {
        wait::Condition::Element { index, state } => {
            let view = backend.view_element(&tree_query(context), *index)?;
            let matched = match state {
                wait::ElementState::Exists => view.is_some(),
                wait::ElementState::Gone => view.is_none(),
                wait::ElementState::Visible => view.as_ref().is_some_and(|view| {
                    view.frame
                        .map(|frame| frame.width > 0 && frame.height > 0)
                        .unwrap_or(false)
                }),
                wait::ElementState::Enabled => view.as_ref().is_some_and(|view| view.enabled),
            };
            Ok((matched, view_observation(view)))
        }
        wait::Condition::Value {
            index,
            needle,
            ignore_case,
        } => {
            let view = backend.view_element(&tree_query(context), *index)?;
            let matched = view
                .as_ref()
                .and_then(|view| view.value.as_deref())
                .is_some_and(|value| wait::contains(value, needle, *ignore_case));
            Ok((matched, view_observation(view)))
        }
        wait::Condition::Text {
            needle,
            ignore_case,
            within_element,
        } => {
            let text = match within_element {
                Some(index) => backend
                    .view_element(&tree_query(context), *index)?
                    .map(|view| view.subtree_text)
                    .unwrap_or_default(),
                None => backend.observe_tree(&tree_query(context))?.full_text,
            };
            let matched = wait::contains(&text, needle, *ignore_case);
            Ok((
                matched,
                json!({"snippet": wait::snippet(&text, needle, *ignore_case)}),
            ))
        }
        wait::Condition::TreeChange => {
            let report = backend.observe_tree(&tree_query(context))?;
            let diff = report.diff.clone().unwrap_or_default();
            let matched = !diff.trim().is_empty();
            Ok((matched, json!({"diff": diff, "revision": report.revision})))
        }
        wait::Condition::Window { state, selector } => {
            let resolved = window_for_wait(backend, context, selector.as_deref())?;
            let matched = match state {
                wait::WindowState::Exists => resolved.is_some(),
                wait::WindowState::Gone => resolved.is_none(),
            };
            Ok((matched, window_observation(resolved)))
        }
        wait::Condition::AppRunning { name, ignore_case } => {
            let apps = backend.list_apps(true)?;
            let found = apps.into_iter().find(|app| {
                wait::contains(&app.name, name, *ignore_case)
                    || app
                        .bundle_id
                        .as_deref()
                        .is_some_and(|bundle| wait::contains(bundle, name, *ignore_case))
                    || app
                        .path
                        .as_deref()
                        .is_some_and(|path| wait::contains(path, name, *ignore_case))
            });
            let observation = found
                .as_ref()
                .map(|app| {
                    json!({
                        "name": app.name,
                        "bundle_id": app.bundle_id,
                        "pid": app.pid,
                    })
                })
                .unwrap_or(Value::Null);
            Ok((found.is_some(), observation))
        }
        wait::Condition::Title {
            needle,
            ignore_case,
        } => {
            let title = title_for_wait(backend, context)?;
            let matched = title
                .as_deref()
                .is_some_and(|title| wait::contains(title, needle, *ignore_case));
            Ok((matched, json!({"title": title})))
        }
    }
}

fn view_observation(view: Option<crate::element::ElementView>) -> Value {
    match view {
        Some(view) => json!({
            "role": view.role,
            "title": view.title,
            "value": view.value,
            "secure": view.secure,
            "enabled": view.enabled,
            "frame": view.frame.map(|frame| json!({
                "x": frame.x,
                "y": frame.y,
                "width": frame.width,
                "height": frame.height,
            })),
        }),
        None => Value::Null,
    }
}

fn window_observation(window: Option<crate::target::WindowInfo>) -> Value {
    match window {
        Some(window) => json!({
            "id": window.id,
            "title": window.title,
            "app": window.app,
            "pid": window.pid,
        }),
        None => Value::Null,
    }
}

fn window_for_wait(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    selector: Option<&str>,
) -> Result<Option<crate::target::WindowInfo>, ComputerError> {
    let listed = backend.list_windows()?;
    let windows = match context.app.as_deref() {
        Some(app) => {
            let parsed = parse_app_selector(app)?;
            let apps = backend.list_apps(true)?;
            let info = resolve_app(&apps, &parsed)?;
            filter_windows_for_pid(&listed.windows, info.pid)
        }
        None => listed.windows,
    };
    let Some(selector) = selector else {
        return Ok(crate::target::hide_system_windows(&windows)
            .into_iter()
            .next());
    };
    let parsed = parse_window_selector(selector)?;
    match resolve_window(&windows, &parsed) {
        Ok(window) => Ok(Some(window.clone())),
        Err(ComputerError::WindowNotFound(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

fn title_for_wait(
    backend: &dyn DesktopBackend,
    context: &CallContext,
) -> Result<Option<String>, ComputerError> {
    if context.window.is_some() || context.app.is_some() {
        return Ok(
            window_for_wait(backend, context, context.window.as_deref())?
                .map(|window| window.title),
        );
    }
    Ok(backend.frontmost().window)
}

impl Action {
    /// A sleep-only `Wait` with no condition. Adapters use it.
    pub fn wait_sleep(ms: Option<u64>, seconds: Option<f64>) -> Self {
        Action::Wait {
            ms,
            seconds,
            element: None,
            element_state: None,
            text: None,
            value: None,
            within_element: None,
            window_state: None,
            tree_change: false,
            app_running: None,
            title: None,
            ignore_case: false,
            timeout_ms: None,
            poll_ms: None,
        }
    }
}

pub(crate) fn resolve_wait(ms: Option<u64>, seconds: Option<f64>) -> Result<u64, ComputerError> {
    if ms.is_none() && seconds.is_none() {
        return Err(ComputerError::InvalidArguments(
            "wait requires ms, seconds, or both".into(),
        ));
    }
    let mut total: u128 = u128::from(ms.unwrap_or(0));
    if let Some(seconds) = seconds {
        if !seconds.is_finite() || seconds < 0.0 {
            return Err(ComputerError::InvalidArguments(
                "seconds must be a finite number greater than or equal to 0".into(),
            ));
        }
        let extra = (seconds * 1000.0).round();
        if extra > u128::MAX as f64 {
            return Err(ComputerError::InvalidArguments("wait is too long".into()));
        }
        total = total.saturating_add(extra as u128);
    }
    if total > MAX_WAIT_MS {
        return Err(ComputerError::InvalidArguments(
            "wait must be at most 24 hours".into(),
        ));
    }
    u64::try_from(total).map_err(|_| ComputerError::InvalidArguments("wait is too long".into()))
}

fn displays(backend: &dyn DesktopBackend) -> Result<Value, ComputerError> {
    let displays = backend.list_displays()?;
    Ok(json!({"displays": displays}))
}

/// Resolve `--display` for a capture. `None` means no filter.
fn display_filter(
    backend: &dyn DesktopBackend,
    context: &CallContext,
) -> Result<Option<crate::isolation::DisplayTarget>, ComputerError> {
    match context.display.as_deref() {
        None => Ok(None),
        Some(spec) => Ok(Some(crate::isolation::resolve(
            spec,
            &backend.list_displays()?,
        )?)),
    }
}

/// Body of the `isolation` action: what this build can and cannot isolate.
fn isolation() -> Result<Value, ComputerError> {
    Ok(crate::isolation::matrix())
}

fn tree_query(context: &CallContext) -> TreeQuery {
    TreeQuery {
        app: context.app.clone(),
        window: context.window.clone(),
        full: context.full,
        diff: context.diff,
        session: context.session.clone(),
    }
}

/// OCR result for a window capture. The dispatcher returns one block list per
/// recognized line. macOS reuses `VNRecognizeTextRequest`, Windows uses
/// `Windows.Media.Ocr.OcrEngine`, and Linux is a structured `Unsupported`.
fn ocr_action(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    _force: bool,
    image_path: Option<&str>,
) -> Result<Value, ComputerError> {
    if let Some(path) = image_path.map(str::trim).filter(|path| !path.is_empty()) {
        return ocr_image(backend, path);
    }
    let targeted = context.app.is_some() || context.window.is_some();
    if !targeted {
        return Err(ComputerError::InvalidArguments(
            "ocr requires --app or --window".into(),
        ));
    }
    let prepared = latest_prepared_ocr_capture(backend, context)?;
    let OcrResult { blocks, note } = backend.recognize_text(&prepared.image, &prepared.map)?;
    let redacted = redact_secure_ocr_blocks(blocks, backend, context, &prepared.map)?;
    let blocks_json: Vec<Value> = redacted
        .iter()
        .map(|block| {
            let image_rect = crate::vision::ocr::ImageRect {
                x: block.x,
                y: block.y,
                width: block.width,
                height: block.height,
            };
            let screen = image_rect.to_screen(&prepared.map);
            let screen_json = match screen {
                Some((x, y)) => json!({
                    "x": x.round() as i64,
                    "y": y.round() as i64,
                }),
                None => Value::Null,
            };
            json!({
                "text": block.text,
                "x": block.x,
                "y": block.y,
                "width": block.width,
                "height": block.height,
                "confidence": block.confidence,
                "screen": screen_json,
            })
        })
        .collect();
    let mut body = json!({
        "action": "ocr",
        "blocks": blocks_json,
    });
    if let Some(note) = note {
        body["note"] = json!(note);
    }
    Ok(body)
}

/// Drop blocks that fall inside any secure element's frame. Re-uses the
/// vision redaction helper so the rule lives in one place. The dispatcher
/// is conservative: when the secure frame list is empty (the typical
/// Linux/Windows case or a tree-unavailable snapshot), every block is kept.
fn redact_secure_ocr_blocks(
    blocks: Vec<OcrBlock>,
    backend: &dyn DesktopBackend,
    context: &CallContext,
    map: &crate::image_util::ScreenshotMap,
) -> Result<Vec<OcrBlock>, ComputerError> {
    // We do not walk the UiNode tree directly; the backend's secure-frame
    // extraction lives in a `secure_ocr_frames()` method. It receives the
    // screenshot map because platform frames are screen points and the OCR
    // blocks are image pixels. The default is an empty list.
    let secure_frames = backend.secure_ocr_frames(&tree_query(context), map)?;
    Ok(crate::vision::redact_blocks(blocks, secure_frames))
}

/// OCR on a file. `screen` equals the image pixel; there is no AX tree to
/// redact against, so the response says so.
fn ocr_image(backend: &dyn DesktopBackend, path: &str) -> Result<Value, ComputerError> {
    let image = image::open(path)
        .map_err(|error| {
            ComputerError::InvalidArguments(format!("could not read image {path}: {error}"))
        })?
        .to_rgba8();
    let (width, height) = image.dimensions();
    let map = crate::image_util::ScreenshotMap::identity(width, height);
    let OcrResult { blocks, note } = backend.recognize_text(&image, &map)?;
    let blocks_json: Vec<Value> = blocks
        .iter()
        .map(|block| {
            let image_rect = crate::vision::ocr::ImageRect {
                x: block.x,
                y: block.y,
                width: block.width,
                height: block.height,
            };
            let screen = image_rect.to_screen(&map);
            json!({
                "text": block.text,
                "x": block.x,
                "y": block.y,
                "width": block.width,
                "height": block.height,
                "confidence": block.confidence,
                "screen": match screen {
                    Some((x, y)) => json!({
                        "x": x.round() as i64,
                        "y": y.round() as i64,
                    }),
                    None => Value::Null,
                },
            })
        })
        .collect();
    let mut body = json!({
        "action": "ocr",
        "source": "image",
        "image": path,
        "width": width,
        "height": height,
        "blocks": blocks_json,
    });
    if let Some(note) = note {
        body["note"] = json!(note);
    }
    Ok(body)
}

/// One OCR-ready capture: the prepared shot pixels plus its screenshot map.
struct OcrPreparedCapture {
    image: image::RgbaImage,
    map: crate::image_util::ScreenshotMap,
}

/// Re-run the same capture the screenshot command uses so OCR has the same
/// letterboxed source. Failures here are returned to the caller, never
/// swallowed.
fn latest_prepared_ocr_capture(
    backend: &dyn DesktopBackend,
    context: &CallContext,
) -> Result<OcrPreparedCapture, ComputerError> {
    // `ocr_action` rejects an untargeted request, so this always captures a
    // window. Keeping one path here means the prepared map matches what the
    // screenshot command would have produced for the same target.
    if let Some(error) = backend.display_block() {
        return Err(error);
    }
    let target = resolve_screenshot_window(backend, context)?;
    let captured = backend.capture_window(&target)?;
    let prepared = image_util::prepare_shot(
        captured.frame.image(),
        image_util::ShotFit {
            scale: captured.frame.scale(),
            origin_x: captured.origin_x,
            origin_y: captured.origin_y,
            logical_width: captured.logical_width,
            logical_height: captured.logical_height,
            max_width: image_util::DEFAULT_SCREENSHOT_MAX_WIDTH,
            canvas: None,
        },
    )?;
    Ok(OcrPreparedCapture {
        image: prepared.image,
        map: prepared.map,
    })
}

/// Draw tree index marks on `image`. Returns the count of marks applied.
///
/// The overlay is painted onto a copy, so the returned image has to be written
/// back into `*image`. Dropping it leaves the caller's pixels untouched and
/// `screenshot --marks` writes a clean capture while still answering `ok`.
fn apply_marks(
    image: &mut image::RgbaImage,
    map: &crate::image_util::ScreenshotMap,
    context: &CallContext,
    backend: &dyn DesktopBackend,
) -> Result<usize, ComputerError> {
    let boxes = backend.marks_for_tree(&tree_query(context), map)?;
    if boxes.is_empty() {
        return Ok(0);
    }
    let marked = crate::vision::draw_marks(image, &boxes);
    *image = marked;
    Ok(boxes.len())
}

fn tree_action(
    backend: &dyn DesktopBackend,
    context: &CallContext,
) -> Result<Value, ComputerError> {
    let report = backend.tree(&tree_query(context))?;
    let mut body = json!({
        "pid": report.pid,
        "window_id": report.window_id,
        "revision": report.revision,
        "source": report.source,
        "truncated": report.truncated,
        "tree": report.tree,
        // `diff` is `None` when this observation created the baseline.
        "baseline": if report.diff.is_some() { "existing" } else { "created" },
        "mode": if report.diff.is_some() { "diff" } else { "full" },
        "diff_requested": context.diff,
    });
    if let Some(note) = report.note {
        body["note"] = json!(note);
    }
    if let Some(legacy) = report.legacy.as_deref() {
        body["accessibility_legacy"] = serde_json::from_str(legacy).unwrap_or(Value::Null);
    }
    let tree_was_empty = report.full_text.trim().is_empty();
    if tree_was_empty {
        // P11: surface the OCR fallback when the tree produced nothing, and
        // a window target was selected. The agent decides whether to run it.
        if context.app.is_some() || context.window.is_some() {
            body["ocr_fallback_available"] = json!(true);
        }
    }
    Ok(body)
}

fn session_end(
    backend: &dyn DesktopBackend,
    context: &CallContext,
) -> Result<Value, ComputerError> {
    let restored = backend.end_session(context.session.as_deref())?;
    let mut body = json!({
        "action": "session_end",
        "restored": restored,
    });
    if let Err(error) = cache::prune_empty_sessions() {
        body["warnings"] = json!([format!("session prune failed: {error}")]);
    }
    Ok(body)
}

fn window_json(window: &crate::target::WindowInfo) -> Value {
    let mut value = serde_json::to_value(window).unwrap_or(Value::Null);
    value["kind"] = json!(crate::target::window_kind(window));
    value["system"] = json!(crate::target::is_system_window(window));
    value
}

/// The resolved window row for the shared request context, or `window_not_found`.
fn require_window_row(
    backend: &dyn DesktopBackend,
    context: &CallContext,
) -> Result<crate::target::WindowInfo, ComputerError> {
    window_for_wait(backend, context, context.window.as_deref())?.ok_or_else(|| {
        let label = context.window.clone().unwrap_or_else(|| {
            context
                .app
                .clone()
                .unwrap_or_else(|| "the front window".into())
        });
        ComputerError::WindowNotFound(label)
    })
}

/// Display id whose frame contains the window's center.
fn display_for_bounds(
    backend: &dyn DesktopBackend,
    bounds: crate::target::WindowBounds,
) -> Option<u32> {
    let center_x = f64::from(bounds.x) + f64::from(bounds.width) / 2.0;
    let center_y = f64::from(bounds.y) + f64::from(bounds.height) / 2.0;
    backend
        .list_displays()
        .ok()?
        .into_iter()
        .find(|display| {
            center_x >= f64::from(display.x)
                && center_x < f64::from(display.x + display.width as i32)
                && center_y >= f64::from(display.y)
                && center_y < f64::from(display.y + display.height as i32)
        })
        .map(|display| display.id)
}

/// The window row after a frame change, plus display and status.
fn window_frame_response(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    label: &str,
) -> Result<Value, ComputerError> {
    let row = require_window_row(backend, context)?;
    let mut body = json!({
        "action": label,
        "window": window_json(&row),
        "display": display_for_bounds(backend, row.bounds),
    });
    if let Ok(status) = backend.window_status(&row.app, context.window.as_deref()) {
        body["state"] = serde_json::to_value(status).unwrap_or(Value::Null);
    }
    Ok(body)
}

/// Clamp a requested frame to the display that contains its center. Honors
/// `[window] clamp`; the default is true.
fn clamp_window_frame(
    backend: &dyn DesktopBackend,
    current: &crate::target::WindowInfo,
    x: Option<f64>,
    y: Option<f64>,
    width: Option<f64>,
    height: Option<f64>,
) -> (Option<f64>, Option<f64>, Option<f64>, Option<f64>) {
    if !policy::window_clamp() {
        return (x, y, width, height);
    }
    let Ok(displays) = backend.list_displays() else {
        return (x, y, width, height);
    };
    let Some(first) = displays.first() else {
        return (x, y, width, height);
    };
    let width = width.unwrap_or(f64::from(current.bounds.width)).max(1.0);
    let height = height.unwrap_or(f64::from(current.bounds.height)).max(1.0);
    let x = x.unwrap_or(f64::from(current.bounds.x));
    let y = y.unwrap_or(f64::from(current.bounds.y));
    let center_x = x + width / 2.0;
    let center_y = y + height / 2.0;
    let display = displays
        .iter()
        .find(|display| {
            center_x >= f64::from(display.x)
                && center_x < f64::from(display.x + display.width as i32)
                && center_y >= f64::from(display.y)
                && center_y < f64::from(display.y + display.height as i32)
        })
        .unwrap_or(first);
    let width = width.min(f64::from(display.width));
    let height = height.min(f64::from(display.height));
    let max_x = (f64::from(display.x) + f64::from(display.width) - width).max(f64::from(display.x));
    let max_y =
        (f64::from(display.y) + f64::from(display.height) - height).max(f64::from(display.y));
    (
        Some(x.clamp(f64::from(display.x), max_x)),
        Some(y.clamp(f64::from(display.y), max_y)),
        Some(width),
        Some(height),
    )
}

fn window_move_action(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    x: Option<i32>,
    y: Option<i32>,
    to_display: Option<u32>,
) -> Result<Value, ComputerError> {
    if to_display.is_some() && (x.is_some() || y.is_some()) {
        return Err(ComputerError::InvalidArguments(
            "window move takes --to-display or --x/--y, not both".into(),
        ));
    }
    let current = require_window_row(backend, context)?;
    let (x, y) = match to_display {
        Some(id) => {
            let displays = backend.list_displays()?;
            let display = displays
                .iter()
                .find(|display| display.id == id)
                .ok_or_else(|| {
                    ComputerError::InvalidArguments(format!("unknown display id {id}"))
                })?;
            let x = display.x + ((display.width as i32 - current.bounds.width as i32) / 2).max(0);
            let y = display.y + ((display.height as i32 - current.bounds.height as i32) / 2).max(0);
            (Some(x), Some(y))
        }
        None => {
            let Some(x) = x else {
                return Err(ComputerError::InvalidArguments(
                    "window move needs --x and --y, or --to-display".into(),
                ));
            };
            let Some(y) = y else {
                return Err(ComputerError::InvalidArguments(
                    "window move needs --x and --y, or --to-display".into(),
                ));
            };
            (Some(x), Some(y))
        }
    };
    let (x, y, width, height) = clamp_window_frame(
        backend,
        &current,
        x.map(f64::from),
        y.map(f64::from),
        None,
        None,
    );
    backend.set_window_frame(&current.app, context.window.as_deref(), x, y, width, height)?;
    window_frame_response(backend, context, "window_move")
}

fn window_set_action(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    x: Option<i32>,
    y: Option<i32>,
    width: Option<u32>,
    height: Option<u32>,
    label: &str,
) -> Result<Value, ComputerError> {
    if x.is_none() && y.is_none() && width.is_none() && height.is_none() {
        return Err(ComputerError::InvalidArguments(
            "window set needs at least one of --x, --y, --width, --height".into(),
        ));
    }
    let current = require_window_row(backend, context)?;
    let (x, y, width, height) = clamp_window_frame(
        backend,
        &current,
        x.map(f64::from),
        y.map(f64::from),
        width.map(f64::from),
        height.map(f64::from),
    );
    backend.set_window_frame(&current.app, context.window.as_deref(), x, y, width, height)?;
    window_frame_response(backend, context, label)
}

fn window_state_action(
    backend: &dyn DesktopBackend,
    context: &CallContext,
    minimized: Option<bool>,
    fullscreen: Option<bool>,
) -> Result<Value, ComputerError> {
    let row = require_window_row(backend, context)?;
    if minimized.is_some() || fullscreen.is_some() {
        backend.set_window_state(&row.app, context.window.as_deref(), minimized, fullscreen)?;
    }
    let status = backend.window_status(&row.app, context.window.as_deref())?;
    Ok(json!({
        "action": "window_state",
        "window": window_json(&row),
        "state": serde_json::to_value(status).unwrap_or(Value::Null),
    }))
}

fn window_info_action(
    backend: &dyn DesktopBackend,
    context: &CallContext,
) -> Result<Value, ComputerError> {
    let row = require_window_row(backend, context)?;
    let mut body = json!({
        "action": "window_info",
        "window": window_json(&row),
        "display": display_for_bounds(backend, row.bounds),
    });
    if let Ok(status) = backend.window_status(&row.app, context.window.as_deref()) {
        body["state"] = serde_json::to_value(status).unwrap_or(Value::Null);
    }
    Ok(body)
}

fn list_apps(
    backend: &dyn DesktopBackend,
    all: bool,
    pattern: Option<&str>,
) -> Result<Value, ComputerError> {
    let apps = backend.list_apps(all)?;
    let hidden = if all {
        0
    } else {
        backend
            .list_apps(true)
            .map(|every| every.len().saturating_sub(apps.len()))
            .unwrap_or(0)
    };
    let mut rows: Vec<Value> = apps
        .iter()
        .filter(|app| app_matches(app, pattern))
        .map(|app| {
            let mut value = serde_json::to_value(app).unwrap_or(Value::Null);
            value["kind"] = json!(crate::target::app_kind(app));
            value["system"] = json!(crate::target::is_system_app(app));
            value
        })
        .collect();
    rows.sort_by(|left, right| {
        left["name"]
            .as_str()
            .unwrap_or_default()
            .cmp(right["name"].as_str().unwrap_or_default())
    });
    let mut body = json!({"apps": rows});
    if hidden > 0 {
        body["hidden_count"] = json!(hidden);
        body["note"] = json!(format!(
            "{hidden} accessory/system apps hidden; pass --all to include them"
        ));
    }
    Ok(body)
}

fn app_matches(app: &crate::target::AppInfo, pattern: Option<&str>) -> bool {
    let Some(pattern) = pattern.map(str::trim).filter(|text| !text.is_empty()) else {
        return true;
    };
    let fields = [
        app.name.as_str(),
        app.bundle_id.as_deref().unwrap_or_default(),
        app.path.as_deref().unwrap_or_default(),
    ];
    if pattern.contains('*') {
        fields
            .iter()
            .any(|field| policy::glob_match(pattern, field))
    } else {
        let needle = pattern.to_ascii_lowercase();
        fields
            .iter()
            .any(|field| field.to_ascii_lowercase().contains(&needle))
    }
}

fn list_windows(
    backend: &dyn DesktopBackend,
    app: Option<&str>,
    all: bool,
) -> Result<Value, ComputerError> {
    let listed = backend.list_windows()?;
    let windows = match app {
        None => listed.windows,
        Some(app) => {
            let selector = parse_app_selector(app)?;
            let apps = backend.list_apps(true)?;
            let info = resolve_app(&apps, &selector)?;
            filter_windows_for_pid(&listed.windows, info.pid)
        }
    };
    let system_count = windows
        .iter()
        .filter(|window| crate::target::is_system_window(window))
        .count();
    let visible = if all {
        windows
    } else {
        crate::target::hide_system_windows(&windows)
    };
    // `index` is what `--window N` resolves, so `--all` output stays
    // addressable. System rows keep their platform `z` and carry no index.
    let mut app_index = 0u32;
    let rows: Vec<Value> = visible
        .iter()
        .map(|window| {
            let mut value = window_json(window);
            if crate::target::is_system_window(window) {
                value["index"] = Value::Null;
            } else {
                value["index"] = json!(app_index);
                app_index += 1;
            }
            value
        })
        .collect();
    let mut body = json!({"windows": rows, "count": rows.len()});
    if !all && system_count > 0 {
        body["system_hidden"] = json!(system_count);
        body["note"] = json!(format!(
            "{system_count} system windows hidden; pass --all to list them, use `index` for `--window`, or reach one with id:<n>"
        ));
    }
    if let Some(note) = listed.note {
        body["note"] = json!(note);
    }
    Ok(body)
}

fn launch(backend: &dyn DesktopBackend, app: Option<&str>) -> Result<Value, ComputerError> {
    let app = require_app(app)?;
    let result = backend.launch(app)?;
    Ok(json!({
        "action": "launch",
        "name": result.name,
        "bundle_id": result.bundle_id,
        "pid": result.pid,
        "path": result.path,
    }))
}

fn activate(
    backend: &dyn DesktopBackend,
    app: Option<&str>,
    window: Option<&str>,
) -> Result<Value, ComputerError> {
    let app = require_app(app)?;
    let result = backend.activate(app, window)?;
    Ok(json!({
        "action": "activate",
        "name": result.name,
        "bundle_id": result.bundle_id,
        "pid": result.pid,
        "path": result.path,
        "window_id": result.window_id,
        "window_title": result.window_title,
    }))
}

fn quit(
    backend: &dyn DesktopBackend,
    app: Option<&str>,
    force: bool,
) -> Result<Value, ComputerError> {
    let app = require_app(app)?;
    let result = backend.quit(app, force)?;
    Ok(json!({
        "action": "quit",
        "name": result.name,
        "bundle_id": result.bundle_id,
        "pid": result.pid,
        "accepted": result.accepted,
        "force": force,
    }))
}

fn doctor(
    backend: &dyn DesktopBackend,
    options: crate::doctor::DoctorOptions,
) -> Result<Value, ComputerError> {
    let report = backend.doctor(options)?;
    serde_json::to_value(report).map_err(|error| {
        ComputerError::ExecutionFailed(format!("failed to encode doctor report: {error}"))
    })
}

fn require_app(app: Option<&str>) -> Result<&str, ComputerError> {
    match app.map(str::trim).filter(|text| !text.is_empty()) {
        Some(app) => Ok(app),
        None => Err(ComputerError::InvalidArguments("app is required".into())),
    }
}

/// JSON result printed by the CLI and returned by [`dispatch`].
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    /// Whether the action completed.
    pub ok: bool,
    /// Request id, when the caller sent one.
    pub id: Option<Value>,
    /// Present when `ok` is false.
    pub error: Option<ErrorBody>,
    /// Extra fields flattened into the JSON object.
    pub fields: Map<String, Value>,
}

impl Response {
    /// Attach an extra top-level field to the response body.
    pub fn with_field(mut self, key: impl Into<String>, value: Value) -> Self {
        self.fields.insert(key.into(), value);
        self
    }

    /// Successful response. `data` must be a JSON object; other values are stored as `result`.
    pub fn success(data: Value) -> Self {
        let fields = match data {
            Value::Object(map) => map,
            other => {
                let mut map = Map::new();
                map.insert("result".into(), other);
                map
            }
        };
        Self {
            ok: true,
            id: None,
            error: None,
            fields,
        }
    }

    /// Failed response with a stable `code` and `message`.
    pub fn failure(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            id: None,
            error: Some(ErrorBody {
                code: code.into(),
                message: message.into(),
            }),
            fields: Map::new(),
        }
    }

    /// Failed response from a [`ComputerError`].
    pub fn from_error(error: ComputerError) -> Self {
        Self::failure(error.code(), error.to_string())
    }

    /// Attach a request id.
    pub fn with_id(mut self, id: Option<Value>) -> Self {
        self.id = id;
        self
    }

    /// JSON object for this response.
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("ok".into(), Value::Bool(self.ok));
        if let Some(id) = &self.id {
            map.insert("id".into(), id.clone());
        }
        if let Some(error) = &self.error {
            map.insert(
                "error".into(),
                serde_json::to_value(error).unwrap_or_else(
                    |_| json!({"code": "execution_failed", "message": "failed to encode error"}),
                ),
            );
        }
        for (key, value) in &self.fields {
            if key != "ok" && key != "id" && key != "error" {
                map.insert(key.clone(), value.clone());
            }
        }
        Value::Object(map)
    }

    /// Serialize as compact or pretty JSON, followed by a newline.
    pub fn to_json_line(&self, pretty: bool) -> String {
        let value = self.to_value();
        let body = if pretty {
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{\"ok\":false}".to_string())
        } else {
            serde_json::to_string(&value).unwrap_or_else(|_| "{\"ok\":false}".to_string())
        };
        format!("{body}\n")
    }
}

impl From<ComputerError> for Response {
    fn from(error: ComputerError) -> Self {
        Self::from_error(error)
    }
}

impl Serialize for Response {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Response {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let Value::Object(mut object) = value else {
            return Err(de::Error::custom("response must be a JSON object"));
        };
        let ok = object
            .remove("ok")
            .ok_or_else(|| de::Error::missing_field("ok"))?;
        let ok = ok
            .as_bool()
            .ok_or_else(|| de::Error::custom("ok must be a boolean"))?;
        let id = object.remove("id");
        let error = match object.remove("error") {
            Some(value) => Some(serde_json::from_value(value).map_err(de::Error::custom)?),
            None => None,
        };
        Ok(Self {
            ok,
            id,
            error,
            fields: object,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_backend::StubBackend;
    use std::sync::atomic::Ordering;

    #[test]
    fn action_roundtrip_and_aliases() {
        let click: Action = serde_json::from_str(
            r#"{"action":"click","x":10,"y":20,"button":"right","double":true}"#,
        )
        .unwrap();
        match &click {
            Action::Click {
                x,
                y,
                button,
                double,
                count,
            } => {
                assert_eq!(
                    (*x, *y, *button, *double, *count),
                    (Some(10), Some(20), MouseButton::Right, true, 1)
                );
            }
            other => panic!("unexpected {other:?}"),
        }
        let encoded = serde_json::to_string(&click).unwrap();
        let decoded: Action = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, click);

        let moved: Action = serde_json::from_str(r#"{"action":"move_mouse","x":1,"y":2}"#).unwrap();
        assert!(matches!(moved, Action::Move { x: 1, y: 2 }));

        let state: Action = serde_json::from_str(r#"{"action":"get-state"}"#).unwrap();
        assert!(matches!(state, Action::GetState));
        let state: Action = serde_json::from_str(r#"{"action":"get_state"}"#).unwrap();
        assert!(matches!(state, Action::GetState));

        let shot: Action = serde_json::from_str(
            r#"{"action":"screenshot","region":"0,0,8,9","format":"jpeg","max_width":640,"base64":true}"#,
        )
        .unwrap();
        match shot {
            Action::Screenshot {
                region,
                format,
                max_width,
                base64,
                ..
            } => {
                assert_eq!(region.unwrap().width, 8);
                assert_eq!(format, Some(ImageFormat::Jpeg));
                assert_eq!(max_width, Some(640));
                assert!(base64);
            }
            other => panic!("unexpected {other:?}"),
        }
        let shot: Action = serde_json::from_str(
            r#"{"action":"screenshot","region":{"x":0,"y":1,"width":2,"height":3}}"#,
        )
        .unwrap();
        match shot {
            Action::Screenshot { region, .. } => {
                assert_eq!(region.unwrap().height, 3);
            }
            other => panic!("unexpected {other:?}"),
        }

        assert!(serde_json::from_str::<Action>(r#"{"action":"fly"}"#).is_err());
        let bare: Action = serde_json::from_str(r#"{"action":"click"}"#).unwrap();
        assert!(matches!(
            bare,
            Action::Click {
                x: None,
                y: None,
                ..
            }
        ));
        let press: Action = serde_json::from_str(r#"{"action":"press"}"#).unwrap();
        assert!(matches!(
            press,
            Action::Press {
                count: 1,
                button: MouseButton::Left
            }
        ));
        let renamed: Action =
            serde_json::from_str(r#"{"action":"set-value","value":"hi"}"#).unwrap();
        assert!(matches!(renamed, Action::SetValue { .. }));
        let scrolled: Action = serde_json::from_str(r#"{"action":"scroll-to-visible"}"#).unwrap();
        assert!(matches!(scrolled, Action::ScrollTo));
    }

    #[test]
    fn response_shape_and_roundtrip() {
        let ok = Response::success(json!({"waited_ms": 1})).with_id(Some(json!(7)));
        let value = ok.to_value();
        assert_eq!(value["ok"], true);
        assert!(value.get("error").is_none());
        assert_eq!(value["waited_ms"], 1);
        assert_eq!(value["id"], 7);
        let text = serde_json::to_string(&ok).unwrap();
        let back: Response = serde_json::from_str(&text).unwrap();
        assert_eq!(back.to_value(), value);

        let err = Response::failure("invalid_arguments", "bad region");
        let value = err.to_value();
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], "invalid_arguments");
        assert_eq!(value["error"]["message"], "bad region");
        assert!(value.get("waited_ms").is_none());
        let line = err.to_json_line(false);
        assert!(line.ends_with('\n'));
        assert!(!line[..line.len() - 1].contains('\n'));
    }

    #[test]
    fn scroll_and_wait_resolution() {
        assert_eq!(
            resolve_scroll(Some(1), Some(-2), None, None).unwrap(),
            (1, -2)
        );
        assert_eq!(
            resolve_scroll(None, None, Some(ScrollDirection::Up), Some(3)).unwrap(),
            (0, -3)
        );
        assert_eq!(
            resolve_scroll(None, None, Some(ScrollDirection::Right), None).unwrap(),
            (1, 0)
        );
        assert_eq!(
            resolve_scroll(None, None, Some(ScrollDirection::Left), Some(4)).unwrap(),
            (-4, 0)
        );
        assert_eq!(
            resolve_scroll(None, None, Some(ScrollDirection::Down), Some(2)).unwrap(),
            (0, 2)
        );
        assert!(resolve_scroll(Some(1), None, Some(ScrollDirection::Up), None).is_err());
        assert!(resolve_scroll(None, None, None, Some(2)).is_err());
        assert!(resolve_scroll(None, None, None, None).is_err());

        assert_eq!(resolve_wait(Some(10), None).unwrap(), 10);
        assert_eq!(resolve_wait(Some(10), Some(0.5)).unwrap(), 510);
        assert_eq!(resolve_wait(None, Some(1.0)).unwrap(), 1000);
        assert!(resolve_wait(None, None).is_err());
        assert!(resolve_wait(None, Some(-1.0)).is_err());
        assert!(resolve_wait(None, Some(f64::NAN)).is_err());
    }

    #[test]
    fn json_line_dispatch_and_stub_actions() {
        let wait = handle_request_line(r#"{"id":1,"action":"wait","ms":0}"#);
        assert!(wait.ok);
        assert_eq!(wait.to_value()["id"], 1);
        assert_eq!(wait.to_value()["waited_ms"], 0);

        let bad = handle_request_line("not-json");
        assert!(!bad.ok);
        assert_eq!(bad.to_value()["error"]["code"], "invalid_arguments");

        let missing = handle_request_line(r#"{"id":"x","action":"nope"}"#);
        assert!(!missing.ok);
        assert_eq!(missing.to_value()["id"], "x");

        let request = parse_request(json!({"id": 3, "action": "displays"})).unwrap();
        assert_eq!(request.id, Some(json!(3)));
        assert!(matches!(request.action, Action::Displays));

        let backend = StubBackend::default();
        let response = dispatch_with(
            &backend,
            Action::Click {
                x: Some(3),
                y: Some(4),
                button: MouseButton::Left,
                double: true,
                count: 1,
            },
        );
        assert!(response.ok);
        assert_eq!(response.to_value()["action"], "double_click");
        assert_eq!(backend.clicks.load(Ordering::SeqCst), 2);

        let response = dispatch_with(&backend, Action::Move { x: 8, y: 9 });
        assert!(response.ok);
        assert_eq!(backend.moves.load(Ordering::SeqCst), 1);

        let response = dispatch_with(&backend, Action::Type { text: "hi".into() });
        assert_eq!(response.to_value()["chars"], 2);
        assert_eq!(backend.typed.lock().unwrap().as_slice(), ["hi"]);

        let response = dispatch_with(
            &backend,
            Action::Key {
                keys: "cmd+shift+4".into(),
            },
        );
        assert!(response.ok);
        assert_eq!(response.to_value()["key"], "4");
        let keys = backend.keys.lock().unwrap();
        assert_eq!(keys[0].1, "4");
        assert_eq!(keys[0].0, vec![Modifier::Command, Modifier::Shift]);
        drop(keys);

        let response = dispatch_with(
            &backend,
            Action::Key {
                keys: "not+a+key".into(),
            },
        );
        assert!(!response.ok);

        let response = dispatch_with(
            &backend,
            Action::Scroll {
                x: Some(1),
                y: Some(2),
                dx: None,
                dy: None,
                direction: Some(ScrollDirection::Down),
                amount: Some(3),
            },
        );
        assert!(response.ok);
        assert_eq!(backend.scrolls.lock().unwrap().as_slice(), [(0, 3)]);
        assert_eq!(backend.moves.load(Ordering::SeqCst), 1);
        assert_eq!(response.to_value()["tier"], "event");
        assert_eq!(response.to_value()["cursor_moved"], false);

        let response = dispatch_with(&backend, Action::Displays);
        assert_eq!(response.to_value()["displays"][0]["name"], "Stub");

        let response = dispatch_with(&backend, Action::GetState);
        assert_eq!(response.to_value()["cursor"]["x"], 4);
        assert!(response.ok);

        let _cache = crate::cache::CacheEnv::acquire();
        let shot = dispatch_with(
            &backend,
            Action::Screenshot {
                output: None,
                region: None,
                format: Some(ImageFormat::Png),
                max_width: Some(1),
                canvas: None,
                base64: true,
                marks: false,
            },
        );
        assert!(shot.ok, "{shot:?}");
        let value = shot.to_value();
        assert_eq!(value["width"], 1);
        assert_eq!(value["height"], 1);
        assert_eq!(value["scale"], 2.0);
        assert_eq!(value["format"], "png");
        let path = value["path"].as_str().unwrap();
        let bytes = fs::read(path).unwrap();
        assert_eq!(
            &bytes[..8],
            &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
        );
        assert!(value["data"].as_str().unwrap().starts_with("iVBOR"));
        assert_eq!(value["capture_source"], "stub");
        assert_eq!(value["origin"]["x"], 0);
        assert_eq!(value["origin"]["y"], 0);
        assert_eq!(value["fit"], "width");
        assert!(value["map"]["image_to_screen"].is_string());
        let _ = fs::remove_file(path);
        if let Ok(dir) = crate::cache::session_dir(None) {
            let _ = fs::remove_file(dir.join("screenshot-map.json"));
        }

        let failed = StubBackend {
            fail_click: true,
            ..StubBackend::default()
        };
        let response = dispatch_with(
            &failed,
            Action::Click {
                x: Some(0),
                y: Some(0),
                button: MouseButton::Left,
                double: false,
                count: 1,
            },
        );
        assert!(!response.ok);
        assert_eq!(response.to_value()["error"]["code"], "execution_failed");
    }

    #[test]
    fn dispatch_wait_does_not_need_a_display() {
        let response = dispatch(Action::wait_sleep(Some(0), None));
        assert!(response.ok);
        assert_eq!(response.to_value()["waited_ms"], 0);
    }

    fn shot(max_width: Option<u32>, canvas: Option<&str>) -> Action {
        Action::Screenshot {
            output: None,
            region: None,
            format: Some(ImageFormat::Png),
            max_width,
            canvas: canvas.map(str::to_string),
            base64: false,
            marks: false,
        }
    }

    #[test]
    fn dispatch_applies_default_max_width_and_keeps_explicit_zero() {
        let _cache = crate::cache::CacheEnv::acquire();
        let backend = StubBackend {
            capture_pixels: Some((1600, 1000)),
            ..StubBackend::default()
        };
        let response = dispatch_with_context(
            &backend,
            shot(None, None),
            &CallContext {
                session: Some("p4-default-width".into()),
                ..CallContext::default()
            },
        );
        assert!(response.ok, "{response:?}");
        let value = response.to_value();
        assert_eq!(value["width"], 1440);
        assert_eq!(value["height"], 900);
        assert_eq!(value["scale"], 2.0);
        assert_eq!(value["logical_width"], 800);
        assert_eq!(value["logical_height"], 500);
        assert_eq!(value["fit"], "width");
        assert_eq!(value["capture_source"], "stub");
        assert_eq!(value["content_width"], 1440);
        let path = value["path"].as_str().unwrap();
        let _ = fs::remove_file(path);
        let map = crate::cache::load_screenshot_map(Some("p4-default-width"))
            .unwrap()
            .expect("map");
        assert_eq!(map.width, 1440);
        let _ = fs::remove_dir_all(crate::cache::session_dir(Some("p4-default-width")).unwrap());

        let full = dispatch_with_context(
            &backend,
            shot(Some(0), None),
            &CallContext {
                session: Some("p4-zero-width".into()),
                ..CallContext::default()
            },
        );
        assert!(full.ok, "{full:?}");
        assert_eq!(full.to_value()["width"], 1600);
        assert_eq!(full.to_value()["height"], 1000);
        assert_eq!(full.to_value()["fit"], "none");
        let path = full.to_value()["path"].as_str().unwrap().to_string();
        let _ = fs::remove_file(path);
        let _ = fs::remove_dir_all(crate::cache::session_dir(Some("p4-zero-width")).unwrap());
    }

    #[test]
    fn dispatch_window_capture_records_origin_and_faults() {
        let _cache = crate::cache::CacheEnv::acquire();
        let backend = StubBackend::default();
        let response = dispatch_with_context(
            &backend,
            shot(Some(0), None),
            &CallContext {
                app: Some("Notes".into()),
                window: Some("0".into()),
                session: Some("p4-window".into()),
                ..CallContext::default()
            },
        );
        assert!(response.ok, "{response:?}");
        let value = response.to_value();
        assert_eq!(value["capture_source"], "stub");
        assert_eq!(value["origin"]["x"], 10);
        assert_eq!(value["origin"]["y"], 20);
        assert_eq!(value["logical_width"], 400);
        assert_eq!(value["logical_height"], 300);
        assert_eq!(value["width"], 800);
        assert_eq!(value["fit"], "none");
        let path = value["path"].as_str().unwrap();
        let _ = fs::remove_file(path);
        let _ = fs::remove_dir_all(crate::cache::session_dir(Some("p4-window")).unwrap());

        let denied = StubBackend {
            capture_fault: crate::test_backend::StubCaptureFault::Permission,
            ..StubBackend::default()
        };
        let response = dispatch_with_context(
            &denied,
            shot(Some(0), None),
            &CallContext {
                app: Some("Notes".into()),
                ..CallContext::default()
            },
        );
        assert!(!response.ok);
        assert_eq!(response.to_value()["error"]["code"], "permission_required");
        assert!(response.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Screen Recording"));

        let missing = StubBackend {
            capture_fault: crate::test_backend::StubCaptureFault::MissingWindow,
            ..StubBackend::default()
        };
        let response = dispatch_with_context(
            &missing,
            shot(Some(0), None),
            &CallContext {
                window: Some("id:100".into()),
                ..CallContext::default()
            },
        );
        assert!(!response.ok);
        assert_eq!(response.to_value()["error"]["code"], "window_not_found");

        let both = dispatch_with_context(
            &backend,
            Action::Screenshot {
                output: None,
                region: Some(Region::new(0, 0, 8, 9).unwrap()),
                format: None,
                max_width: Some(0),
                canvas: None,
                base64: false,
                marks: false,
            },
            &CallContext {
                app: Some("Notes".into()),
                ..CallContext::default()
            },
        );
        assert_eq!(both.to_value()["error"]["code"], "invalid_arguments");

        let locked = StubBackend {
            block_display: true,
            windows: Vec::new(),
            ..StubBackend::default()
        };
        let response = dispatch_with_context(
            &locked,
            shot(Some(0), None),
            &CallContext {
                app: Some("Notes".into()),
                ..CallContext::default()
            },
        );
        assert_eq!(response.to_value()["error"]["code"], "display_unavailable");
        assert!(response.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("locked"));
    }

    #[test]
    fn shared_flags_are_stripped_before_the_action_decodes() {
        let request = parse_request(json!({
            "id": 4,
            "action": "list_windows",
            "app": "Foo \"Bar\"",
            "window": 0,
            "element": "3",
            "diff": true,
            "full": false,
            "return_state": true,
            "require_visible": true
        }))
        .unwrap();
        assert_eq!(request.id, Some(json!(4)));
        assert!(matches!(request.action, Action::ListWindows { .. }));
        assert_eq!(request.context.app.as_deref(), Some("Foo \"Bar\""));
        assert_eq!(request.context.window.as_deref(), Some("0"));
        assert_eq!(request.context.element, Some(3));
        assert!(request.context.diff);
        assert!(!request.context.full);
        assert!(request.context.return_state);
        assert!(request.context.require_visible);
    }

    fn ctx(app: &str) -> CallContext {
        CallContext {
            app: Some(app.to_string()),
            ..CallContext::default()
        }
    }

    /// A confirm hook that always allows. Returns the command and a directory
    /// the caller can remove.
    fn allow_confirm_hook() -> (String, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "computer-use-confirm-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let allow = dir.join("allow.sh");
            std::fs::write(&allow, "#!/bin/sh\ncat >/dev/null\nexit 0\n").unwrap();
            std::fs::set_permissions(&allow, std::fs::Permissions::from_mode(0o700)).unwrap();
            (allow.display().to_string(), dir)
        }
        #[cfg(windows)]
        {
            ("cmd /C exit 0".to_string(), dir)
        }
    }

    fn view(role: &str) -> crate::element::ElementView {
        crate::element::ElementView {
            role: role.into(),
            title: Some(role.into()),
            value: Some("ready".into()),
            secure: false,
            enabled: true,
            frame: Some(crate::tree::SavedFrame {
                x: 10,
                y: 10,
                width: 80,
                height: 20,
            }),
            subtree_text: format!("{role} ready"),
        }
    }

    fn wait_action_with(
        element: Option<u32>,
        element_state: Option<&str>,
        text: Option<&str>,
        value: Option<&str>,
        within_element: Option<u32>,
    ) -> Action {
        Action::Wait {
            ms: None,
            seconds: None,
            element,
            element_state: element_state.map(str::to_string),
            text: text.map(str::to_string),
            value: value.map(str::to_string),
            within_element,
            window_state: None,
            tree_change: false,
            app_running: None,
            title: None,
            ignore_case: false,
            timeout_ms: Some(150),
            poll_ms: Some(25),
        }
    }

    #[test]
    fn wait_rejects_conflicting_or_incomplete_conditions() {
        let backend = StubBackend::default();
        let two = dispatch_with_context(
            &backend,
            wait_action_with(Some(1), None, Some("hello"), None, None),
            &ctx("Notes"),
        );
        assert_eq!(two.to_value()["error"]["code"], "invalid_arguments");

        let value_without_element = dispatch_with_context(
            &backend,
            wait_action_with(None, None, None, Some("x"), None),
            &ctx("Notes"),
        );
        assert_eq!(
            value_without_element.to_value()["error"]["code"],
            "invalid_arguments"
        );

        let state_without_element = dispatch_with_context(
            &backend,
            wait_action_with(None, Some("visible"), None, None, None),
            &ctx("Notes"),
        );
        assert_eq!(
            state_without_element.to_value()["error"]["code"],
            "invalid_arguments"
        );

        let within_without_text = dispatch_with_context(
            &backend,
            wait_action_with(None, None, None, None, Some(2)),
            &ctx("Notes"),
        );
        assert_eq!(
            within_without_text.to_value()["error"]["code"],
            "invalid_arguments"
        );
    }

    #[test]
    fn wait_value_reads_the_element_it_names() {
        let backend = StubBackend::default();
        backend
            .element_views
            .lock()
            .unwrap()
            .extend([None, Some(view("AXTextField"))]);
        // `--element` provides the index; `--value` is the single condition,
        // so the pair is accepted.
        let response = dispatch_with_context(
            &backend,
            wait_action_with(Some(3), None, None, Some("ready"), None),
            &ctx("Notes"),
        );
        assert!(response.ok, "{:?}", response.to_value());
        assert_eq!(response.to_value()["condition"], "value");
    }

    fn wait_with(app_running: Option<&str>, title: Option<&str>) -> Action {
        Action::Wait {
            ms: None,
            seconds: None,
            element: None,
            element_state: None,
            text: None,
            value: None,
            within_element: None,
            window_state: None,
            tree_change: false,
            app_running: app_running.map(str::to_string),
            title: title.map(str::to_string),
            ignore_case: true,
            timeout_ms: Some(200),
            poll_ms: Some(25),
        }
    }

    #[test]
    fn wait_app_running_matches_name_bundle_and_path() {
        let backend = StubBackend::default();
        for needle in ["Notes", "com.apple.Notes", "/Applications/Notes.app"] {
            let response =
                dispatch_with_context(&backend, wait_with(Some(needle), None), &ctx("Notes"));
            assert!(response.ok, "{needle}: {:?}", response.to_value());
            assert_eq!(response.to_value()["condition"], "app_running");
            assert_eq!(response.to_value()["observed"]["name"], "Notes");
        }
        let missing = dispatch_with_context(
            &backend,
            wait_with(Some("no-such-app-xyz"), None),
            &ctx("Notes"),
        );
        assert_eq!(missing.to_value()["error"]["code"], "timeout");
        assert!(missing.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no-such-app-xyz"));
    }

    #[test]
    fn wait_title_matches_the_target_window() {
        let backend = StubBackend::default();
        // The Notes fixture window is titled "Shopping".
        let found = dispatch_with_context(&backend, wait_with(None, Some("shop")), &ctx("Notes"));
        assert!(found.ok, "{:?}", found.to_value());
        assert_eq!(found.to_value()["condition"], "title");
        assert_eq!(found.to_value()["observed"]["title"], "Shopping");

        let missing =
            dispatch_with_context(&backend, wait_with(None, Some("nothing")), &ctx("Notes"));
        assert_eq!(missing.to_value()["error"]["code"], "timeout");
    }

    #[test]
    fn window_set_reports_the_new_frame() {
        let backend = StubBackend::default();
        let response = dispatch_with_context(
            &backend,
            Action::WindowSet {
                x: Some(10),
                y: None,
                width: Some(200),
                height: None,
            },
            &ctx("Notes"),
        );
        assert!(response.ok, "{:?}", response.to_value());
        assert_eq!(response.to_value()["action"], "window_set");
        let calls = backend.window_calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert!(calls[0].contains("x=10"), "{}", calls[0]);
        assert!(calls[0].contains("width=200"), "{}", calls[0]);
    }

    #[test]
    fn image_coordinates_cover_double_triple_drag_and_regions() {
        let cache = crate::cache::CacheEnv::acquire();
        assert!(cache.dir().is_dir());
        let map = sample_map();
        cache::store_screenshot_map(Some("p5-map-more"), &map).unwrap();
        let backend = StubBackend::default();
        let context = CallContext {
            coord: Some("image".into()),
            session: Some("p5-map-more".into()),
            ..CallContext::default()
        };

        let double = dispatch_with_context(
            &backend,
            Action::DoubleClick {
                x: Some(100),
                y: Some(50),
                button: MouseButton::Left,
            },
            &context,
        );
        assert!(double.ok, "{:?}", double.to_value());
        assert_eq!(double.to_value()["x"], 176);

        let triple = dispatch_with_context(
            &backend,
            Action::TripleClick {
                x: Some(100),
                y: Some(50),
                button: MouseButton::Left,
            },
            &context,
        );
        assert!(triple.ok, "{:?}", triple.to_value());

        let drag = dispatch_with_context(
            &backend,
            Action::Drag {
                path: vec![Point { x: 100, y: 50 }, Point { x: 110, y: 60 }],
                button: MouseButton::Left,
            },
            &context,
        );
        assert!(drag.ok, "{:?}", drag.to_value());

        let mapped = remap_region(&context, Region::new(10, 10, 20, 20).unwrap()).unwrap();
        assert!(mapped.width >= 1 && mapped.height >= 1);
        let outside = remap_region(&context, Region::new(1400, 10, 100, 10).unwrap()).unwrap_err();
        assert_eq!(outside.code(), "invalid_arguments");
        assert!(outside.to_string().contains("outside the screenshot"));
    }

    #[test]
    fn image_coordinates_cover_click_mouse_scroll_screenshot_and_zoom() {
        let cache = crate::cache::CacheEnv::acquire();
        let map = sample_map();
        cache::store_screenshot_map(Some("p5-map-arms"), &map).unwrap();
        let backend = StubBackend::default();
        let context = CallContext {
            coord: Some("image".into()),
            session: Some("p5-map-arms".into()),
            ..CallContext::default()
        };
        let shot = cache.dir().join("arms.png");

        let click = dispatch_with_context(
            &backend,
            Action::Click {
                x: Some(100),
                y: Some(50),
                button: MouseButton::Left,
                double: false,
                count: 1,
            },
            &context,
        );
        assert!(click.ok, "{:?}", click.to_value());
        assert_eq!(click.to_value()["x"], 176);

        let down = dispatch_with_context(
            &backend,
            Action::MouseDown {
                x: 100,
                y: 50,
                button: MouseButton::Left,
            },
            &context,
        );
        assert!(down.ok, "{:?}", down.to_value());
        let up = dispatch_with_context(
            &backend,
            Action::MouseUp {
                x: 100,
                y: 50,
                button: MouseButton::Left,
            },
            &context,
        );
        assert!(up.ok, "{:?}", up.to_value());

        let scroll = dispatch_with_context(
            &backend,
            Action::Scroll {
                x: Some(100),
                y: Some(50),
                dx: Some(0),
                dy: Some(10),
                direction: None,
                amount: None,
            },
            &context,
        );
        assert!(scroll.ok, "{:?}", scroll.to_value());

        let region = Region::new(10, 10, 20, 20).unwrap();
        // A capture stores its own map, so restore the sample map before each
        // region-remapping action.
        cache::store_screenshot_map(Some("p5-map-arms"), &map).unwrap();
        let screenshot = dispatch_with_context(
            &backend,
            Action::Screenshot {
                output: Some(shot.display().to_string()),
                region: Some(region),
                format: None,
                max_width: None,
                canvas: None,
                base64: false,
                marks: false,
            },
            &context,
        );
        assert!(screenshot.ok, "{:?}", screenshot.to_value());
        cache::store_screenshot_map(Some("p5-map-arms"), &map).unwrap();
        let zoom = dispatch_with_context(
            &backend,
            Action::Zoom {
                output: Some(shot.display().to_string()),
                region,
                format: None,
                max_width: None,
                canvas: None,
                base64: false,
            },
            &context,
        );
        assert!(zoom.ok, "{:?}", zoom.to_value());

        // An action with no coordinates passes through unchanged.
        let key = dispatch_with_context(
            &backend,
            Action::Key {
                keys: "cmd+s".into(),
            },
            &context,
        );
        assert!(key.ok, "{:?}", key.to_value());
        let _ = std::fs::remove_dir_all(cache.dir());
    }

    #[test]
    fn clipboard_get_reports_an_image() {
        let cache = crate::cache::CacheEnv::acquire();
        let backend = StubBackend::default();
        *backend.clipboard_image.lock().unwrap() = Some(vec![10, 20, 30, 255]);
        let response = dispatch_with_context(&backend, Action::ClipboardGet, &ctx("Notes"));
        assert!(response.ok, "{:?}", response.to_value());
        let value = response.to_value();
        assert_eq!(value["kind"], "image");
        assert!(value["path"]
            .as_str()
            .unwrap_or_default()
            .ends_with("clipboard.png"));
        assert_eq!(value["width"], 1);
        let _ = std::fs::remove_dir_all(cache.dir());
    }

    #[test]
    fn batch_steps_overlay_context_keys() {
        let backend = StubBackend::default();
        let response = dispatch_with_context(
            &backend,
            Action::Batch {
                actions: vec![json!({
                    "action": "move",
                    "x": 1,
                    "y": 2,
                    "app": "Notes",
                    "window": "0",
                    "element": 3,
                    "diff": true,
                    "full": true,
                    "coord": "screen"
                })],
                stop_on_error: true,
            },
            &ctx("Notes"),
        );
        assert!(response.ok, "{:?}", response.to_value());
        assert_eq!(response.to_value()["results"][0]["ok"], true);
    }

    #[test]
    fn round_screen_rejects_non_finite_and_out_of_range() {
        assert_eq!(round_screen(1.6).unwrap(), 2);
        assert_eq!(
            round_screen(f64::NAN).unwrap_err().code(),
            "invalid_arguments"
        );
        assert_eq!(
            round_screen(f64::INFINITY).unwrap_err().code(),
            "invalid_arguments"
        );
        assert_eq!(
            round_screen(f64::from(i32::MAX) + 1.0).unwrap_err().code(),
            "invalid_arguments"
        );
    }

    #[test]
    fn ocr_action_reports_blocks() {
        let backend = StubBackend {
            capture_pixels: Some((400, 300)),
            ..StubBackend::default()
        };
        *backend.ocr_blocks.lock().unwrap() = Some(vec![OcrBlock {
            text: "hello".into(),
            x: 10,
            y: 10,
            width: 20,
            height: 10,
            confidence: 0.8,
        }]);
        let response = dispatch_with_context(
            &backend,
            Action::Ocr {
                force: false,
                image: None,
            },
            &ctx("Notes"),
        );
        assert!(response.ok, "{:?}", response.to_value());
        let value = response.to_value();
        assert_eq!(value["action"], "ocr");
        assert_eq!(value["blocks"][0]["text"], "hello");
        assert!(value["blocks"][0]["screen"]["x"].is_number(), "{value}");
    }

    #[test]
    fn clipboard_set_image_decodes_a_png() {
        let (allow, dir) = allow_confirm_hook();
        let backend = StubBackend::default();
        let mut context = ctx("Notes");
        context.confirm_cmd = Some(allow);
        let fixture = format!(
            "{}/tests/fixtures/ocr/hello.png",
            env!("CARGO_MANIFEST_DIR")
        );
        let response = dispatch_with_context(
            &backend,
            Action::ClipboardSet {
                text: None,
                image: Some(fixture.clone()),
            },
            &context,
        );
        assert!(response.ok, "{:?}", response.to_value());
        let value = response.to_value();
        assert_eq!(value["kind"], "image");
        assert_eq!(value["width"], 960);
        assert_eq!(value["height"], 240);

        let missing = dispatch_with_context(
            &backend,
            Action::ClipboardSet {
                text: None,
                image: Some("/definitely/missing.png".into()),
            },
            &context,
        );
        assert_eq!(missing.to_value()["error"]["code"], "invalid_arguments");

        let corrupt = dir.join("corrupt.png");
        std::fs::write(&corrupt, b"not a png").unwrap();
        let undecodable = dispatch_with_context(
            &backend,
            Action::ClipboardSet {
                text: None,
                image: Some(corrupt.display().to_string()),
            },
            &context,
        );
        assert_eq!(undecodable.to_value()["error"]["code"], "invalid_arguments");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn confirm_title_requires_a_hook() {
        let error = confirm_title(&ctx("Notes"), "press", "sensitive control title").unwrap_err();
        assert_eq!(error.code(), "confirm_required");

        let (allow, dir) = allow_confirm_hook();
        let mut context = ctx("Notes");
        context.confirm_cmd = Some(allow);
        confirm_title(&context, "press", "sensitive control title").unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn synthetic_tier_is_refused_without_the_feature() {
        if crate::tier::skylight_compiled() {
            // With the feature the stub backend would record the synthetic
            // request instead of posting; that path is covered elsewhere.
            return;
        }
        let backend = StubBackend::default();
        let mut context = ctx("Notes");
        context.tier = Some("synthetic".into());
        let response = dispatch_with_context(
            &backend,
            Action::Click {
                x: Some(1),
                y: Some(1),
                button: MouseButton::Left,
                double: false,
                count: 1,
            },
            &context,
        );
        assert_eq!(response.to_value()["error"]["code"], "unsupported");
        assert!(response.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("skylight"));
    }

    #[test]
    fn unknown_fields_are_rejected_on_actions_with_fields() {
        // A typo such as `timeout` instead of `timeout_ms` must not be
        // silently ignored and fall back to the default.
        let error = parse_request(json!({
            "action": "wait",
            "ms": 1,
            "timeout": 5000
        }))
        .unwrap_err();
        assert_eq!(error.code(), "invalid_arguments");
        assert!(error.to_string().contains("unknown field"), "{error}");
    }

    #[test]
    fn parse_request_context_carries_every_flag() {
        let request = parse_request(json!({
            "action": "displays",
            "app": "Notes",
            "window": "0",
            "element": 3,
            "diff": true,
            "full": true,
            "state_diff": true,
            "include_screenshot": true,
            "coord": "image",
            "session": "s1",
            "tier": "ax",
            "adapter": "native",
            "display": "1",
            "require_visible": true,
            "allow_foreground": true,
            "require_background": true,
            "no_overlay": true,
            "cdp": true,
            "cdp_url": "ws://127.0.0.1:1",
            "browser_blocked_urls": ["https://accounts.example.com/*"],
            "on_dialog": "accept",
            "agent_overlay": "on"
        }))
        .unwrap();
        assert!(matches!(request.action, Action::Displays));
        let context = request.context;
        assert_eq!(context.app.as_deref(), Some("Notes"));
        assert_eq!(context.window.as_deref(), Some("0"));
        assert_eq!(context.element, Some(3));
        assert!(context.diff && context.full);
        assert!(context.return_state && context.state_diff);
        assert!(context.include_screenshot);
        assert_eq!(context.coord.as_deref(), Some("image"));
        assert_eq!(context.session.as_deref(), Some("s1"));
        assert_eq!(context.tier.as_deref(), Some("ax"));
        assert_eq!(context.adapter.as_deref(), Some("native"));
        assert_eq!(context.display.as_deref(), Some("1"));
        assert!(context.require_visible);
        assert!(context.allow_foreground && context.require_background);
        assert!(context.no_overlay && context.cdp);
        assert_eq!(context.cdp_url.as_deref(), Some("ws://127.0.0.1:1"));
        assert_eq!(
            context.browser_blocked_urls,
            vec!["https://accounts.example.com/*"]
        );
        assert_eq!(context.on_dialog.as_deref(), Some("accept"));
        assert_eq!(context.agent_overlay.as_deref(), Some("on"));
    }

    #[test]
    fn post_state_warns_when_the_tree_fails() {
        let backend = StubBackend {
            fail_accessibility: true,
            ..StubBackend::default()
        };
        let mut context = ctx("Notes");
        context.return_state = true;
        let response = dispatch_with_context(&backend, Action::Move { x: 5, y: 5 }, &context);
        assert!(response.ok, "{:?}", response.to_value());
        let value = response.to_value();
        let warnings = value["warnings"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .unwrap_or_default();
        assert!(warnings.contains("tree snapshot failed"), "{value}");
        assert!(value["state"]["tree"].is_null(), "{value}");
    }

    #[test]
    fn wait_takes_a_duration_or_a_condition_not_both() {
        let backend = StubBackend::default();
        let mixed = dispatch_with_context(
            &backend,
            Action::Wait {
                ms: Some(5),
                seconds: Some(0.0),
                element: None,
                element_state: None,
                text: Some("x".into()),
                value: None,
                within_element: None,
                window_state: None,
                tree_change: false,
                app_running: None,
                title: None,
                ignore_case: false,
                timeout_ms: Some(150),
                poll_ms: Some(25),
            },
            &ctx("Notes"),
        );
        let value = mixed.to_value();
        assert_eq!(value["error"]["code"], "invalid_arguments");
        assert!(value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("duration or a condition"));
    }

    #[test]
    fn json_wait_and_verify_keep_element_as_a_condition() {
        let wait = parse_request(json!({
            "action": "wait",
            "element": 12,
            "element_state": "visible"
        }))
        .unwrap();
        assert!(
            matches!(
                wait.action,
                Action::Wait {
                    element: Some(12),
                    ..
                }
            ),
            "{:?}",
            wait.action
        );
        assert_eq!(wait.context.element, None);

        let verify = parse_request(json!({"action": "verify", "element": 3})).unwrap();
        assert!(
            matches!(
                verify.action,
                Action::Verify {
                    element: Some(3),
                    ..
                }
            ),
            "{:?}",
            verify.action
        );
        assert_eq!(verify.context.element, None);

        // Every other action still routes `element` into the shared context.
        let press = parse_request(json!({"action": "press", "element": 7})).unwrap();
        assert!(matches!(press.action, Action::Press { .. }));
        assert_eq!(press.context.element, Some(7));
    }

    #[test]
    fn batch_step_keeps_the_wait_element_condition() {
        let backend = StubBackend::default();
        let response = dispatch_with_context(
            &backend,
            Action::Batch {
                actions: vec![json!({
                    "action": "wait",
                    "element": 1,
                    "element_state": "bogus"
                })],
                stop_on_error: true,
            },
            &ctx("Notes"),
        );
        let value = response.to_value();
        // The nested action saw the element; the state is what failed.
        assert!(
            value["results"][0]["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("element state"),
            "{value:?}"
        );
    }

    #[test]
    fn wait_text_observes_without_consuming_the_baseline() {
        let backend = StubBackend::default();
        backend
            .tree_values
            .lock()
            .unwrap()
            .extend(["idle".to_string(), "Ready".to_string()]);
        let response = dispatch_with_context(
            &backend,
            wait_action_with(None, None, Some("Ready"), None, None),
            &ctx("Notes"),
        );
        assert!(response.ok, "{:?}", response.to_value());
        assert!(backend.tree_observations.load(Ordering::SeqCst) >= 1);
        assert_eq!(
            backend.tree_stores.load(Ordering::SeqCst),
            0,
            "a wait loop must not write the session baseline"
        );
    }

    #[test]
    fn tree_change_compares_against_the_caller_baseline() {
        let backend = StubBackend::default();
        let context = ctx("Notes");
        *backend.stub_value.lock().unwrap() = "a".into();
        let seed = backend.tree(&tree_query(&context)).unwrap();
        assert!(
            seed.diff.is_none(),
            "the seed observation created a baseline"
        );

        *backend.stub_value.lock().unwrap() = "b".into();
        let stores = backend.tree_stores.load(Ordering::SeqCst);
        let response = dispatch_with_context(
            &backend,
            Action::Wait {
                ms: None,
                seconds: None,
                element: None,
                element_state: None,
                text: None,
                value: None,
                within_element: None,
                window_state: None,
                tree_change: true,
                app_running: None,
                title: None,
                ignore_case: false,
                timeout_ms: Some(150),
                poll_ms: Some(25),
            },
            &context,
        );
        assert!(response.ok, "{:?}", response.to_value());
        assert_eq!(response.to_value()["condition"], "tree_change");
        assert_eq!(
            backend.tree_stores.load(Ordering::SeqCst),
            stores,
            "the wait must leave the caller's baseline in place"
        );
        assert!(backend.tree_observations.load(Ordering::SeqCst) >= 1);
    }

    #[test]
    fn plain_tree_still_stores_the_baseline() {
        let backend = StubBackend::default();
        let context = ctx("Notes");
        let first = dispatch_with_context(&backend, Action::Tree, &context);
        assert!(first.ok, "{:?}", first.to_value());
        assert_eq!(backend.tree_stores.load(Ordering::SeqCst), 1);
        assert_eq!(backend.tree_observations.load(Ordering::SeqCst), 0);

        let second = dispatch_with_context(&backend, Action::Tree, &context);
        assert!(second.ok, "{:?}", second.to_value());
        assert_eq!(backend.tree_stores.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn wait_text_with_multibyte_prefix_does_not_panic() {
        let backend = StubBackend::default();
        // Curly quotes and an ellipsis are 3-byte characters; the snippet
        // window starts inside them before the match.
        *backend.stub_value.lock().unwrap() = "Saved “Draft”… now".into();
        let response = dispatch_with_context(
            &backend,
            wait_action_with(None, None, Some("Saved"), None, None),
            &ctx("Notes"),
        );
        assert!(response.ok, "{:?}", response.to_value());
        let snippet = response.to_value()["observed"]["snippet"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(snippet.contains("Saved"), "{snippet:?}");
    }

    #[test]
    fn verification_failed_is_a_computer_error_code() {
        assert_eq!(
            ComputerError::VerificationFailed("nope".into()).code(),
            "verification_failed"
        );
    }

    #[test]
    fn wait_text_returns_when_it_appears() {
        let backend = StubBackend::default();
        backend
            .tree_values
            .lock()
            .unwrap()
            .extend(["idle".to_string(), "Ready".to_string()]);
        let response = dispatch_with_context(
            &backend,
            Action::Wait {
                ms: None,
                seconds: None,
                element: None,
                element_state: None,
                text: Some("Ready".into()),
                value: None,
                within_element: None,
                window_state: None,
                tree_change: false,
                app_running: None,
                title: None,
                ignore_case: false,
                timeout_ms: Some(1_000),
                poll_ms: Some(25),
            },
            &ctx("Notes"),
        );
        assert!(response.ok, "{:?}", response.to_value());
        let value = response.to_value();
        assert_eq!(value["condition"], "text");
        assert!(value["observed"]["snippet"]
            .as_str()
            .unwrap_or_default()
            .contains("Ready"));
    }

    #[test]
    fn wait_element_appears_and_times_out() {
        let backend = StubBackend::default();
        backend
            .element_views
            .lock()
            .unwrap()
            .extend([None, Some(view("AXButton"))]);
        let response = dispatch_with_context(
            &backend,
            Action::Wait {
                ms: None,
                seconds: None,
                element: Some(2),
                element_state: None,
                text: None,
                value: None,
                within_element: None,
                window_state: None,
                tree_change: false,
                app_running: None,
                title: None,
                ignore_case: false,
                timeout_ms: Some(1_000),
                poll_ms: Some(25),
            },
            &ctx("Notes"),
        );
        assert!(response.ok, "{:?}", response.to_value());
        assert_eq!(response.to_value()["observed"]["role"], "AXButton");

        let empty = StubBackend::default();
        let timeout = dispatch_with_context(
            &empty,
            Action::Wait {
                ms: None,
                seconds: None,
                element: Some(9),
                element_state: None,
                text: None,
                value: None,
                within_element: None,
                window_state: None,
                tree_change: false,
                app_running: None,
                title: None,
                ignore_case: false,
                timeout_ms: Some(150),
                poll_ms: Some(25),
            },
            &ctx("Notes"),
        );
        assert_eq!(timeout.to_value()["error"]["code"], "timeout");
    }

    #[test]
    fn verify_passes_and_fails_with_its_own_code() {
        let backend = StubBackend::default();
        backend
            .tree_values
            .lock()
            .unwrap()
            .extend(["idle".to_string(), "Ready".to_string()]);
        let pass = dispatch_with_context(
            &backend,
            Action::Verify {
                element: None,
                element_state: None,
                text: Some("Ready".into()),
                value: None,
                within_element: None,
                window_state: None,
                tree_change: false,
                app_running: None,
                title: None,
                ignore_case: false,
                timeout_ms: Some(1_000),
                poll_ms: Some(25),
            },
            &ctx("Notes"),
        );
        assert!(pass.ok, "{:?}", pass.to_value());
        assert_eq!(pass.to_value()["verified"], true);

        let fail = dispatch_with_context(
            &backend,
            Action::Verify {
                element: None,
                element_state: None,
                text: Some("Never".into()),
                value: None,
                within_element: None,
                window_state: None,
                tree_change: false,
                app_running: None,
                title: None,
                ignore_case: false,
                timeout_ms: Some(150),
                poll_ms: Some(25),
            },
            &ctx("Notes"),
        );
        let value = fail.to_value();
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], "verification_failed");
        assert!(value.get("observed").is_some(), "{value:?}");
    }

    #[test]
    fn window_move_centers_on_a_display() {
        let backend = StubBackend::default();
        let response = dispatch_with_context(
            &backend,
            Action::WindowMove {
                x: None,
                y: None,
                to_display: Some(1),
            },
            &ctx("Notes"),
        );
        assert!(response.ok, "{:?}", response.to_value());
        let value = response.to_value();
        assert_eq!(value["action"], "window_move");
        assert_eq!(value["display"], 1);
        assert_eq!(value["window"]["id"], 100);
        let calls = backend.window_calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert!(calls[0].contains("x=200"), "{}", calls[0]);
        assert!(calls[0].contains("y=150"), "{}", calls[0]);
    }

    #[test]
    fn window_move_rejects_display_and_coordinates_together() {
        let backend = StubBackend::default();
        let response = dispatch_with_context(
            &backend,
            Action::WindowMove {
                x: Some(10),
                y: Some(20),
                to_display: Some(1),
            },
            &ctx("Notes"),
        );
        assert_eq!(response.to_value()["error"]["code"], "invalid_arguments");
        assert!(backend.window_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn window_move_requires_coordinates_or_display() {
        let backend = StubBackend::default();
        let response = dispatch_with_context(
            &backend,
            Action::WindowMove {
                x: Some(10),
                y: None,
                to_display: None,
            },
            &ctx("Notes"),
        );
        assert_eq!(response.to_value()["error"]["code"], "invalid_arguments");
        assert!(backend.window_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn window_resize_clamps_to_the_display() {
        let backend = StubBackend::default();
        let response = dispatch_with_context(
            &backend,
            Action::WindowResize {
                width: 900,
                height: 900,
            },
            &ctx("Notes"),
        );
        assert!(response.ok, "{:?}", response.to_value());
        let calls = backend.window_calls.lock().unwrap().clone();
        assert!(calls[0].contains("width=800"), "{}", calls[0]);
        assert!(calls[0].contains("height=600"), "{}", calls[0]);
    }

    #[test]
    fn window_state_sets_and_reports() {
        let backend = StubBackend::default();
        *backend.window_status_fixture.lock().unwrap() = crate::target::WindowStatus {
            minimized: Some(true),
            fullscreen: Some(false),
            main: Some(true),
            focused: Some(false),
        };
        let response = dispatch_with_context(
            &backend,
            Action::WindowState {
                minimized: Some(true),
                fullscreen: None,
            },
            &ctx("Notes"),
        );
        assert!(response.ok, "{:?}", response.to_value());
        assert_eq!(response.to_value()["state"]["minimized"], true);
        let calls = backend.window_calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].contains("minimized=Some(true)"), "{}", calls[0]);

        // A report-only call touches nothing.
        let backend = StubBackend::default();
        *backend.window_status_fixture.lock().unwrap() = crate::target::WindowStatus {
            minimized: Some(false),
            ..Default::default()
        };
        let response = dispatch_with_context(
            &backend,
            Action::WindowState {
                minimized: None,
                fullscreen: None,
            },
            &ctx("Notes"),
        );
        assert!(response.ok, "{:?}", response.to_value());
        assert_eq!(response.to_value()["state"]["minimized"], false);
        assert!(backend.window_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn window_info_reports_bounds_and_status() {
        let backend = StubBackend::default();
        *backend.window_status_fixture.lock().unwrap() = crate::target::WindowStatus {
            fullscreen: Some(false),
            ..Default::default()
        };
        let response = dispatch_with_context(&backend, Action::WindowInfo, &ctx("Notes"));
        assert!(response.ok, "{:?}", response.to_value());
        let value = response.to_value();
        assert_eq!(value["action"], "window_info");
        assert_eq!(value["window"]["id"], 100);
        assert_eq!(value["window"]["bounds"]["width"], 400);
        assert_eq!(value["display"], 1);
        assert_eq!(value["state"]["fullscreen"], false);
    }

    #[test]
    fn ocr_image_reports_image_pixels_as_screen() {
        let dir =
            std::env::temp_dir().join(format!("computer-use-ocr-image-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shot.png");
        let image = image::RgbaImage::from_pixel(40, 20, image::Rgba([255, 255, 255, 255]));
        image.save(&path).unwrap();

        let backend = StubBackend::default();
        *backend.ocr_blocks.lock().unwrap() = Some(vec![crate::vision::OcrBlock {
            text: "Hello".into(),
            x: 5,
            y: 6,
            width: 10,
            height: 8,
            confidence: 0.9,
        }]);
        let response = dispatch_with(
            &backend,
            Action::Ocr {
                force: false,
                image: Some(path.display().to_string()),
            },
        );
        assert!(response.ok, "{:?}", response.to_value());
        let value = response.to_value();
        assert_eq!(value["source"], "image");
        assert_eq!(value["width"], 40);
        assert_eq!(value["height"], 20);
        assert_eq!(value["blocks"][0]["text"], "Hello");
        // `screen` is the block center, matching every other OCR path.
        assert_eq!(value["blocks"][0]["screen"]["x"], 10);
        assert_eq!(value["blocks"][0]["screen"]["y"], 10);
        assert_eq!(value["note"], "stub");

        let missing = dispatch_with(
            &backend,
            Action::Ocr {
                force: false,
                image: Some("/nonexistent/shot.png".into()),
            },
        );
        assert_eq!(missing.to_value()["error"]["code"], "invalid_arguments");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn type_refuses_a_secure_focused_field() {
        let backend = StubBackend::default();
        *backend.focused.lock().unwrap() = Some(crate::element::ElementPeek {
            title: Some("Password".into()),
            secure: true,
        });
        let response = dispatch_with(
            &backend,
            Action::Type {
                text: "secret".into(),
            },
        );
        assert!(!response.ok, "{:?}", response.to_value());
        assert_eq!(response.to_value()["error"]["code"], "policy_denied");
        assert!(backend.typed.lock().unwrap().is_empty());
        // Enter is allowed: it submits, it does not insert text.
        let enter = dispatch_with(
            &backend,
            Action::Key {
                keys: "enter".into(),
            },
        );
        assert!(enter.ok, "{:?}", enter.to_value());
        // Paste is refused.
        let paste = dispatch_with(
            &backend,
            Action::Key {
                keys: "cmd+v".into(),
            },
        );
        assert_eq!(paste.to_value()["error"]["code"], "policy_denied");
    }

    #[test]
    fn unreadable_focus_warns_and_proceeds() {
        let backend = StubBackend {
            fail_focused: true,
            ..StubBackend::default()
        };
        let response = dispatch_with(
            &backend,
            Action::Type {
                text: "hello".into(),
            },
        );
        assert!(response.ok, "{:?}", response.to_value());
        let warnings = response
            .to_value()
            .get("warnings")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let joined = match warnings {
            serde_json::Value::Array(items) => items
                .iter()
                .filter_map(|item| item.as_str())
                .collect::<Vec<_>>()
                .join("; "),
            serde_json::Value::String(text) => text,
            _ => String::new(),
        };
        assert!(
            joined.contains("secure-field check unavailable"),
            "{:?}",
            response.to_value()
        );
    }

    #[test]
    fn sensitive_menu_item_requires_confirmation() {
        let backend = StubBackend::default();
        let response = dispatch_with_context(
            &backend,
            Action::MenuClick {
                app: Some("Notes".into()),
                path: "File > Empty Trash".into(),
            },
            &ctx("Notes"),
        );
        assert_eq!(response.to_value()["error"]["code"], "confirm_required");
        assert!(backend.menu_clicks.lock().unwrap().is_empty());

        let (allow, dir) = allow_confirm_hook();
        let mut context = ctx("Notes");
        context.confirm_cmd = Some(allow);
        let allowed = dispatch_with_context(
            &backend,
            Action::MenuClick {
                app: Some("Notes".into()),
                path: "File > Empty Trash".into(),
            },
            &context,
        );
        assert!(allowed.ok, "{:?}", allowed.to_value());
        assert_eq!(backend.menu_clicks.lock().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn get_state_degrades_when_frontmost_is_denied() {
        let backend = StubBackend {
            frontmost_app: Some("Terminal".into()),
            ..StubBackend::default()
        };
        let response = dispatch_with(&backend, Action::GetState);
        let value = response.to_value();
        assert_eq!(value["ok"], true, "{value:?}");
        assert_eq!(value["frontmost_app"], "Terminal");
        assert!(value["tree"].is_null(), "{value:?}");
        assert_eq!(value["policy"]["decision"], "deny", "{value:?}");
        assert!(
            value["note"]
                .as_str()
                .unwrap_or_default()
                .contains("deny list"),
            "{value:?}"
        );
    }

    #[test]
    fn script_actions_require_confirmation_by_default() {
        let backend = StubBackend::default();
        let response = dispatch_with(
            &backend,
            Action::AppleEvent {
                app: Some("Notes".into()),
                script: "get name".into(),
                language: None,
            },
        );
        assert!(!response.ok);
        assert_eq!(response.to_value()["error"]["code"], "confirm_required");
        assert!(backend.apple_event_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn list_windows_hides_system_rows_by_default() {
        let mut backend = StubBackend::default();
        backend.windows.insert(
            0,
            crate::target::WindowInfo {
                id: 900,
                pid: 99,
                app: "Window Server".into(),
                title: "Menubar".into(),
                z: 0,
                bounds: crate::target::WindowBounds {
                    x: 0,
                    y: 0,
                    width: 1440,
                    height: 24,
                },
                layer: 25,
                on_screen: true,
                ax_matched: false,
            },
        );

        let response = dispatch_with(&backend, Action::ListWindows { all: false });
        let value = response.to_value();
        let windows = value["windows"].as_array().unwrap();
        assert_eq!(windows.len(), 3, "{value:?}");
        assert!(windows.iter().all(|window| window["system"] == false));
        assert!(windows.iter().all(|window| window["kind"] == "app"));
        assert_eq!(value["system_hidden"], 1);
        assert!(value["note"].as_str().unwrap().contains("--all"));
        // Every visible row is addressable and `index` agrees with `z`.
        for (position, row) in windows.iter().enumerate() {
            assert_eq!(row["index"], position as u64, "{row:?}");
            assert_eq!(row["z"], position as u64, "{row:?}");
        }

        let response = dispatch_with(&backend, Action::ListWindows { all: true });
        let value = response.to_value();
        let windows = value["windows"].as_array().unwrap();
        assert_eq!(windows.len(), 4, "{value:?}");
        assert!(windows.iter().any(|window| window["system"] == true));
        assert!(windows.iter().any(|window| window["kind"] == "panel"));
        assert!(value.get("system_hidden").is_none());
        // The panel is z 0 and carries no index; the app rows keep the same
        // index they have in the default view, so `--window N` still matches.
        let panel = &windows[0];
        assert_eq!(panel["z"], 0);
        assert!(panel["index"].is_null(), "{panel:?}");
        let app_indexes: Vec<u64> = windows
            .iter()
            .filter_map(|row| row["index"].as_u64())
            .collect();
        assert_eq!(app_indexes, vec![0, 1, 2], "{windows:?}");

        // Index resolution skips the system row: window 0 is the Notes window.
        let listed = backend.list_windows().unwrap();
        let selector = crate::target::WindowSelector::Index(0);
        let resolved = crate::target::resolve_window(&listed.windows, &selector).unwrap();
        assert_eq!(resolved.id, 100);
        // `id:` still reaches the system row.
        let system = crate::target::WindowSelector::Id(900);
        let resolved = crate::target::resolve_window(&listed.windows, &system).unwrap();
        assert_eq!(resolved.id, 900);
    }

    #[test]
    fn list_apps_reports_kind_and_supports_match() {
        let backend = StubBackend::default();

        let response = dispatch_with(
            &backend,
            Action::ListApps {
                all: false,
                r#match: None,
            },
        );
        let value = response.to_value();
        assert_eq!(value["hidden_count"], 1, "{value:?}");
        assert!(value["note"].as_str().unwrap().contains("--all"));
        // Rows are sorted by name; `active` marks the frontmost app.
        let names: Vec<&str> = value["apps"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|app| app["name"].as_str())
            .collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "{value:?}");

        let response = dispatch_with(
            &backend,
            Action::ListApps {
                all: false,
                r#match: Some("term".into()),
            },
        );
        let value = response.to_value();
        let apps = value["apps"].as_array().unwrap();
        assert_eq!(apps.len(), 1, "{value:?}");
        assert_eq!(apps[0]["name"], "Terminal");
        assert_eq!(apps[0]["kind"], "regular");
        assert_eq!(apps[0]["system"], false);

        let response = dispatch_with(
            &backend,
            Action::ListApps {
                all: true,
                r#match: Some("*otes*".into()),
            },
        );
        let apps = response.to_value()["apps"].clone();
        let apps = apps.as_array().unwrap();
        assert!(!apps.is_empty());
        assert!(apps.iter().all(|app| app["name"] == "Notes"));

        let response = dispatch_with(
            &backend,
            Action::ListApps {
                all: true,
                r#match: None,
            },
        );
        let value = response.to_value();
        assert!(value["apps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|app| app["kind"] == "accessory"));
        assert!(value.get("hidden_count").is_none());
    }

    #[test]
    fn stub_inventory_launch_and_doctor() {
        let backend = StubBackend::default();

        let response = dispatch_with(
            &backend,
            Action::ListApps {
                all: false,
                r#match: None,
            },
        );
        assert!(response.ok);
        let value = response.to_value();
        let apps = value["apps"].as_array().unwrap();
        assert!(apps.iter().any(|app| app["name"] == "Notes"));
        assert!(apps.iter().all(|app| app["name"] != "Widget"));

        let response = dispatch_with(
            &backend,
            Action::ListApps {
                all: true,
                r#match: None,
            },
        );
        let value = response.to_value();
        assert!(value["apps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|app| app["pid"] == 77));

        let response =
            dispatch_with_context(&backend, Action::ListWindows { all: false }, &ctx("Notes"));
        assert!(response.ok, "{response:?}");
        let value = response.to_value();
        let windows = &value["windows"];
        assert_eq!(windows.as_array().unwrap().len(), 2);
        assert_eq!(windows[0]["z"], 0);
        assert_eq!(windows[0]["id"], 100);
        assert_eq!(windows[0]["index"], 0);
        assert_eq!(windows[1]["z"], 1);
        assert_eq!(windows[1]["index"], 1);
        assert_eq!(windows[1]["title"], "Welcome");
        assert_eq!(windows[0]["ax_matched"], true);
        assert!(value.get("note").is_none());

        let response = dispatch_with_context(&backend, Action::Launch, &ctx("Notes"));
        assert!(response.ok, "{response:?}");
        assert_eq!(response.to_value()["pid"], 10);
        assert_eq!(response.to_value()["action"], "launch");
        assert_eq!(backend.launched.lock().unwrap().as_slice(), ["Notes"]);

        let response = dispatch_with_context(&backend, Action::Launch, &ctx("com.apple.Terminal"));
        assert!(!response.ok);
        assert_eq!(response.to_value()["error"]["code"], "policy_denied");
        assert_eq!(backend.launched.lock().unwrap().len(), 1);

        let response = dispatch_with_context(&backend, Action::Activate, &ctx("10"));
        assert!(response.ok, "{response:?}");
        assert!(response.to_value()["window_id"].is_null());
        assert_eq!(backend.activated.lock().unwrap().len(), 1);

        let mut context = ctx("Notes");
        context.window = Some("Welcome".into());
        let response = dispatch_with_context(&backend, Action::Activate, &context);
        assert_eq!(response.to_value()["window_id"], 101);
        assert_eq!(response.to_value()["window_title"], "Welcome");

        let response =
            dispatch_with_context(&backend, Action::Quit { force: false }, &ctx("Terminal"));
        assert_eq!(response.to_value()["error"]["code"], "policy_denied");
        assert!(backend.quit.lock().unwrap().is_empty());

        let response = dispatch_with_context(&backend, Action::Quit { force: false }, &ctx("412"));
        assert_eq!(response.to_value()["error"]["code"], "confirm_required");
        assert!(backend.quit.lock().unwrap().is_empty());

        let response = dispatch_with(&backend, Action::Launch);
        assert_eq!(response.to_value()["error"]["code"], "invalid_arguments");

        let response = dispatch_with(
            &backend,
            Action::Doctor {
                prompt: false,
                open_settings: false,
                x11: false,
                full: false,
            },
        );
        let value = response.to_value();
        assert_eq!(value["ok"], true);
        assert_eq!(value["permissions_ok"], false);
        assert_eq!(value["accessibility"], true);
        assert_eq!(value["screen_capture"], false);
        assert_eq!(value["screen_recording"], false);
        assert!(value["input_monitor"].is_null());
        assert_eq!(value["missing"][0], "screen_recording");
        assert_eq!(value["responsible_app"]["name"], "Terminal");
        assert!(value["fixes"][0]
            .as_str()
            .unwrap()
            .contains("Screen Recording"));
        assert_eq!(value["fix"][0]["target"], "Terminal");

        let prompted: Action =
            serde_json::from_str(r#"{"action":"doctor","request":true}"#).unwrap();
        assert!(matches!(
            prompted,
            Action::Doctor {
                prompt: true,
                open_settings: false,
                ..
            }
        ));
        let full: Action = serde_json::from_str(r#"{"action":"doctor","full":true}"#).unwrap();
        assert!(matches!(full, Action::Doctor { full: true, .. }));
        let plain: Action = serde_json::from_str(r#"{"action":"doctor"}"#).unwrap();
        assert!(matches!(
            plain,
            Action::Doctor {
                prompt: false,
                open_settings: false,
                ..
            }
        ));

        let response = dispatch_with(&backend, Action::GetState);
        assert!(response.ok);
        let value = response.to_value();
        assert_eq!(value["pid"], 10);
        assert_eq!(value["source"], "stub");
        assert!(value["tree"].as_str().unwrap().contains("AXWindow"));
        assert!(value["accessibility_legacy"].is_null());

        let response = dispatch_with_context(&backend, Action::GetState, &ctx("412"));
        assert_eq!(response.to_value()["pid"], 412);
        assert_eq!(response.to_value()["source"], "stub");
        assert!(response.to_value()["tree"]
            .as_str()
            .unwrap()
            .contains("AXWindow"));
        let seen = backend.accessibility_apps.lock().unwrap().clone();
        assert_eq!(seen.last().and_then(|name| name.as_deref()), Some("412"));

        // Get-state already stored a baseline. Clear it so this sequence
        // still covers "no baseline prints the full tree".
        backend.baselines.lock().unwrap().clear();
        let first = dispatch_with_context(&backend, Action::Tree, &ctx("Notes"));
        assert!(first.ok, "{first:?}");
        let full = first.to_value()["tree"].as_str().unwrap().to_string();
        assert!(full.contains("value=\"a\""), "{full}");
        assert_eq!(first.to_value()["source"], "stub");
        *backend.stub_value.lock().unwrap() = "ab".into();
        let diff = dispatch_with_context(&backend, Action::Tree, &ctx("Notes"));
        let diff_text = diff.to_value()["tree"].as_str().unwrap().to_string();
        assert!(
            diff_text.contains("~ [1] AXTextArea value: \"a\" -> \"ab\""),
            "{diff_text}"
        );
        let mut full_context = ctx("Notes");
        full_context.full = true;
        let replaced = dispatch_with_context(&backend, Action::Tree, &full_context);
        let replaced_text = replaced.to_value()["tree"].as_str().unwrap().to_string();
        assert!(replaced_text.contains("value=\"ab\""), "{replaced_text}");
        assert!(!replaced_text.contains("~ [1]"));
        let after = dispatch_with_context(&backend, Action::Tree, &ctx("Notes"));
        assert_eq!(after.to_value()["tree"].as_str().unwrap(), "");

        let _cache = crate::cache::CacheEnv::acquire();
        let ended = dispatch_with(&backend, Action::SessionEnd);
        assert!(ended.ok, "{ended:?}");
        assert_eq!(ended.to_value()["action"], "session_end");
        assert!(ended.to_value()["restored"].as_array().unwrap().is_empty());
    }

    #[test]
    fn stub_refuses_privacy_window_and_self_quit() {
        let mut backend = StubBackend::default();
        backend.apps.push(crate::target::AppInfo {
            name: "System Settings".into(),
            bundle_id: Some("com.apple.systempreferences".into()),
            pid: 50,
            path: Some(
                "/System/Applications/System Settings.app/Contents/MacOS/System Settings".into(),
            ),
            active: false,
            hidden: false,
            activation_policy: crate::target::ActivationPolicy::Regular,
        });
        backend.windows.push(crate::target::WindowInfo {
            id: 9,
            pid: 50,
            app: "System Settings".into(),
            title: "Privacy & Security".into(),
            z: 4,
            bounds: crate::target::WindowBounds {
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            },
            layer: 0,
            on_screen: true,
            ax_matched: false,
        });
        let mut context = ctx("com.apple.systempreferences");
        context.window = Some("Privacy".into());
        let response = dispatch_with_context(&backend, Action::Activate, &context);
        assert_eq!(response.to_value()["error"]["code"], "policy_denied");
        assert!(backend.activated.lock().unwrap().is_empty());

        let response = dispatch_with_context(&backend, Action::Activate, &ctx("System Settings"));
        assert!(response.ok, "{response:?}");

        let self_pid = std::process::id() as i32;
        backend.apps.retain(|app| app.pid != self_pid);
        backend.apps.push(crate::target::AppInfo {
            name: "computer-use".into(),
            bundle_id: None,
            pid: self_pid,
            path: None,
            active: false,
            hidden: false,
            activation_policy: crate::target::ActivationPolicy::Regular,
        });
        let response = dispatch_with_context(
            &backend,
            Action::Quit { force: false },
            &ctx(&self_pid.to_string()),
        );
        assert_eq!(response.to_value()["error"]["code"], "policy_denied");
    }

    fn notes_button() -> crate::tree::UiNode {
        let mut button = crate::tree::UiNode::bare("AXButton");
        button.title = Some("Save".into());
        button.actions = vec!["AXPress".into(), "AXShowMenu".into()];
        button.frame = Some(crate::tree::NodeFrame {
            x: 12.0,
            y: 10.0,
            width: 80.0,
            height: 24.0,
        });
        let mut root = crate::tree::UiNode::bare("AXWindow");
        root.title = Some("Notes".into());
        root.frame = Some(crate::tree::NodeFrame {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        });
        root.children = vec![button];
        root
    }

    fn element_context(action_element: u32) -> CallContext {
        CallContext {
            app: Some("Notes".into()),
            element: Some(action_element),
            ..CallContext::default()
        }
    }

    #[test]
    fn element_json_shape_revalidation_and_hit_test() {
        let backend = StubBackend::default();
        backend.seed_element(notes_button());
        let request = parse_request(json!({
            "action": "press",
            "app": "Notes",
            "element": 1,
            "count": 1
        }))
        .unwrap();
        let response = dispatch_with_context(&backend, request.action, &request.context);
        assert!(response.ok, "{response:?}");
        let value = response.to_value();
        assert_eq!(value["action"], "press");
        assert_eq!(value["element"], 1);
        assert_eq!(value["tier"], "ax");
        assert_eq!(value["count"], 1);
        assert!(value["revision"].is_number());
        assert!(value.get("warnings").is_none());

        let request = parse_request(json!({
            "action": "perform-action",
            "app": "Notes",
            "element": 1,
            "name": "AXShowMenu"
        }))
        .unwrap();
        let response = dispatch_with_context(&backend, request.action, &request.context);
        assert!(response.ok, "{response:?}");
        assert_eq!(response.to_value()["action"], "perform_action");
        assert_eq!(response.to_value()["name"], "AXShowMenu");
        assert_eq!(response.to_value()["tier"], "ax");

        let before = backend.performs.lock().unwrap().len();
        let request = parse_request(json!({
            "action": "perform_action",
            "app": "Notes",
            "element": 1,
            "name": "AXDelete"
        }))
        .unwrap();
        let response = dispatch_with_context(&backend, request.action, &request.context);
        assert_eq!(response.to_value()["error"]["code"], "invalid_arguments");
        assert_eq!(backend.performs.lock().unwrap().len(), before);

        let request = parse_request(json!({
            "action": "set_value",
            "app": "Notes",
            "element": 1,
            "value": "hello"
        }))
        .unwrap();
        let response = dispatch_with_context(&backend, request.action, &request.context);
        assert!(response.ok, "{response:?}");
        assert_eq!(response.to_value()["action"], "set_value");

        let request = parse_request(json!({
            "action": "select_text",
            "app": "Notes",
            "element": 1,
            "start": 0,
            "end": 5
        }))
        .unwrap();
        let response = dispatch_with_context(&backend, request.action, &request.context);
        assert!(response.ok, "{response:?}");
        assert_eq!(response.to_value()["action"], "select_text");

        let request = parse_request(json!({
            "action": "scroll_to_visible",
            "app": "Notes",
            "element": 1
        }))
        .unwrap();
        assert!(matches!(request.action, Action::ScrollTo));
        let response = dispatch_with_context(&backend, request.action, &request.context);
        assert_eq!(response.to_value()["error"]["code"], "execution_failed");
        assert!(response.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("AXScrollToVisible"));

        let clicks = backend.clicks.load(Ordering::SeqCst);
        let request = parse_request(json!({
            "action": "click",
            "app": "Notes",
            "element": 1,
            "double": true
        }))
        .unwrap();
        let response = dispatch_with_context(&backend, request.action, &request.context);
        assert!(response.ok, "{response:?}");
        assert_eq!(response.to_value()["action"], "press");
        assert_eq!(response.to_value()["count"], 2);
        assert_eq!(backend.clicks.load(Ordering::SeqCst), clicks);

        *backend.hit.lock().unwrap() = Some(crate::element::HitInfo {
            role: "AXButton".into(),
            title: Some("Save".into()),
        });
        let response = dispatch_with(
            &backend,
            Action::Click {
                x: Some(12),
                y: Some(10),
                button: MouseButton::Left,
                double: false,
                count: 1,
            },
        );
        assert!(response.ok, "{response:?}");
        assert_eq!(response.to_value()["hit"]["role"], "AXButton");
        assert_eq!(response.to_value()["hit"]["title"], "Save");
        assert_eq!(backend.clicks.load(Ordering::SeqCst), clicks + 1);

        let mut changed = notes_button();
        changed.children[0].role = "AXTextArea".into();
        backend.set_live(changed);
        let response = dispatch_with_context(&backend, Action::Focus, &element_context(1));
        assert_eq!(response.to_value()["error"]["code"], "stale_element");
        assert!(response.to_value()["tree"]
            .as_str()
            .unwrap()
            .contains("AXTextArea"));

        let empty = StubBackend::default();
        let response = dispatch_with_context(&empty, Action::Focus, &element_context(0));
        assert_eq!(response.to_value()["error"]["code"], "element_not_found");
        assert!(response.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("refresh indices"));

        let bare = dispatch_with(
            &empty,
            Action::Click {
                x: None,
                y: None,
                button: MouseButton::Left,
                double: false,
                count: 1,
            },
        );
        assert_eq!(bare.to_value()["error"]["code"], "invalid_arguments");
    }

    #[test]
    fn element_action_does_not_replace_the_tree_diff_baseline() {
        let backend = StubBackend::default();
        let first = dispatch_with_context(&backend, Action::Tree, &ctx("Notes"));
        assert!(first.ok, "{first:?}");
        let key = (String::new(), 10, 100);
        let baseline = backend.baselines.lock().unwrap().get(&key).cloned();
        let response = dispatch_with_context(&backend, Action::Focus, &element_context(0));
        assert!(response.ok, "{response:?}");
        assert_eq!(
            backend.baselines.lock().unwrap().get(&key),
            baseline.as_ref()
        );
        *backend.stub_value.lock().unwrap() = "ab".into();
        let diff = dispatch_with_context(&backend, Action::Tree, &ctx("Notes"));
        let text = diff.to_value()["tree"].as_str().unwrap().to_string();
        assert!(
            text.contains("~ [1] AXTextArea value: \"a\" -> \"ab\""),
            "{text}"
        );
    }

    fn sample_map() -> crate::image_util::ScreenshotMap {
        crate::image_util::ScreenshotMap {
            width: 1440,
            height: 900,
            scale: 2.0,
            logical_width: 800,
            logical_height: 500,
            origin: crate::image_util::MapPoint { x: 120, y: 40 },
            fit: "width".into(),
            pad: crate::image_util::MapPoint { x: 0, y: 0 },
            content_width: 1440,
            content_height: 900,
            map: crate::image_util::MapFormulas {
                image_to_screen:
                    "screen = origin + (image - pad) * logical_size / image_content_size".into(),
                screen_to_image:
                    "image = pad + (screen - origin) * image_content_size / logical_size".into(),
            },
        }
    }

    #[test]
    fn secure_ocr_frames_receives_the_map_of_the_ocr_capture() {
        // Platform frames are screen points; OCR blocks are image pixels. The
        // dispatcher must hand the backend the screenshot map of the capture
        // the OCR ran on, or redaction compares two different spaces.
        let cache = crate::cache::CacheEnv::acquire();
        assert!(cache.dir().is_dir());
        let backend = StubBackend::default();
        *backend.secure_frames.lock().unwrap() = Some(vec![crate::vision::ocr::ImageRect {
            x: 10,
            y: 20,
            width: 100,
            height: 40,
        }]);
        let context = CallContext {
            app: Some("Finder".into()),
            session: Some("p12b-redact".into()),
            ..CallContext::default()
        };
        let blocks = vec![OcrBlock {
            text: "hunter2".into(),
            x: 20,
            y: 30,
            width: 30,
            height: 10,
            confidence: 0.9,
        }];
        let map = sample_map();
        let kept = redact_secure_ocr_blocks(blocks.clone(), &backend, &context, &map).unwrap();
        assert!(kept.is_empty(), "the block sits inside the secure frame");

        let seen = backend
            .secure_frames_map
            .lock()
            .unwrap()
            .clone()
            .expect("secure_ocr_frames was called");
        assert_eq!(seen.origin, map.origin);
        assert_eq!(seen.logical_width, map.logical_width);
        assert_eq!(seen.content_width, map.content_width);

        let outside = vec![OcrBlock {
            text: "visible".into(),
            x: 900,
            y: 700,
            width: 30,
            height: 10,
            confidence: 0.9,
        }];
        let kept = redact_secure_ocr_blocks(outside, &backend, &context, &map).unwrap();
        assert_eq!(kept.len(), 1);
    }

    /// Screenshot with `--marks` on a real window answered `ok` and wrote an
    /// unmarked PNG. `apply_marks` painted the overlay onto a copy of the image
    /// and threw the copy away, so the encoded bytes never changed.
    #[test]
    fn marks_change_the_pixels_the_screenshot_writes() {
        let cache = crate::cache::CacheEnv::acquire();
        let backend = StubBackend::default();
        let context = CallContext {
            app: Some("Notes".into()),
            session: Some("marks".into()),
            ..CallContext::default()
        };
        let plain = cache.dir().join("plain.png");
        let marked = cache.dir().join("marked.png");

        let shot = |path: std::path::PathBuf, marks: bool| {
            screenshot(
                &backend,
                &context,
                ScreenshotArgs {
                    output: Some(path.to_string_lossy().into_owned()),
                    region: None,
                    format: Some(ImageFormat::Png),
                    max_width: None,
                    canvas: None,
                    include_base64: false,
                    window_crop: false,
                    marks,
                },
            )
        };

        let clean = shot(plain.clone(), false).expect("plain screenshot");
        let marked_body = shot(marked.clone(), true).expect("marked screenshot");

        let clean_bytes = std::fs::read(&plain).expect("plain png");
        let marked_bytes = std::fs::read(&marked).expect("marked png");
        assert_ne!(
            clean_bytes, marked_bytes,
            "--marks wrote the same image as a plain screenshot"
        );

        // The map the backend converted frames with must be the map of this
        // capture, otherwise screen points are read as image pixels.
        let seen = backend
            .marks_map
            .lock()
            .unwrap()
            .clone()
            .expect("marks_for_tree was called");
        assert_eq!(
            i64::from(seen.origin.x),
            clean["origin"]["x"].as_i64().unwrap()
        );
        assert_eq!(
            i64::from(seen.origin.y),
            clean["origin"]["y"].as_i64().unwrap()
        );
        assert_eq!(
            seen.content_width,
            clean["content_width"].as_u64().unwrap() as u32
        );
        assert_eq!(
            seen.logical_width,
            clean["logical_width"].as_u64().unwrap() as u32
        );

        // Marks were applied, so this is not the "nothing to draw" warning.
        let warnings = marked_body["warnings"].as_array();
        assert!(
            !warnings
                .map(|items| items
                    .iter()
                    .any(|item| item.as_str().is_some_and(|text| text.starts_with("marks:"))))
                .unwrap_or(false),
            "unexpected marks warning: {warnings:?}"
        );
    }

    #[test]
    fn marks_warn_when_no_frame_is_inside_the_capture() {
        // A silent zero is indistinguishable from "the marks were drawn".
        let cache = crate::cache::CacheEnv::acquire();
        let backend = StubBackend::default();
        // A tree whose window sits far away from the captured screen: every
        // frame is off-canvas, so nothing can be drawn.
        backend.set_live({
            let mut window = crate::tree::UiNode::bare("AXWindow");
            window.frame = Some(crate::tree::NodeFrame {
                x: 9_000.0,
                y: 9_000.0,
                width: 100.0,
                height: 100.0,
            });
            window
        });
        let context = CallContext {
            session: Some("marks-empty".into()),
            ..CallContext::default()
        };
        let body = screenshot(
            &backend,
            &context,
            ScreenshotArgs {
                output: Some(
                    cache
                        .dir()
                        .join("marks-empty.png")
                        .to_string_lossy()
                        .into_owned(),
                ),
                region: None,
                format: Some(ImageFormat::Png),
                max_width: None,
                canvas: None,
                include_base64: false,
                window_crop: false,
                marks: true,
            },
        )
        .expect("screenshot");
        let warnings = body["warnings"].as_array().expect("warnings");
        assert!(
            warnings
                .iter()
                .any(|item| item.as_str().is_some_and(|text| text.starts_with("marks:"))),
            "expected a marks warning, got {warnings:?}"
        );
    }

    #[test]
    fn no_secure_frames_keeps_every_ocr_block() {
        let cache = crate::cache::CacheEnv::acquire();
        assert!(cache.dir().is_dir());
        let backend = StubBackend::default();
        let context = CallContext {
            app: Some("Finder".into()),
            session: Some("p12b-redact-none".into()),
            ..CallContext::default()
        };
        let blocks = vec![OcrBlock {
            text: "hello".into(),
            x: 1,
            y: 2,
            width: 3,
            height: 4,
            confidence: 0.9,
        }];
        let kept = redact_secure_ocr_blocks(blocks, &backend, &context, &sample_map()).unwrap();
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn image_coordinate_on_a_stored_map_becomes_the_screen_point() {
        let cache = crate::cache::CacheEnv::acquire();
        assert!(cache.dir().is_dir());
        let map = sample_map();
        cache::store_screenshot_map(Some("p5-map"), &map).unwrap();
        let backend = StubBackend::default();
        let context = CallContext {
            coord: Some("image".into()),
            session: Some("p5-map".into()),
            ..CallContext::default()
        };
        let response = dispatch_with_context(&backend, Action::Move { x: 100, y: 50 }, &context);
        assert!(response.ok, "{response:?}");
        let (screen_x, screen_y) = image_util::image_to_screen(&map, 100.0, 50.0);
        let value = response.to_value();
        assert_eq!(value["x"], json!(screen_x.round() as i64));
        assert_eq!(value["y"], json!(screen_y.round() as i64));
        assert_eq!(value["x"], 176);
        assert_eq!(value["y"], 68);
        assert_eq!(backend.moves.load(Ordering::SeqCst), 1);

        let outside = dispatch_with_context(&backend, Action::Move { x: 1440, y: 50 }, &context);
        assert_eq!(outside.to_value()["error"]["code"], "invalid_arguments");
        assert!(outside.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("outside"));
        assert_eq!(backend.moves.load(Ordering::SeqCst), 1);

        let missing = dispatch_with_context(
            &backend,
            Action::Move { x: 1, y: 2 },
            &CallContext {
                coord: Some("image".into()),
                session: Some("no-map".into()),
                ..CallContext::default()
            },
        );
        assert_eq!(missing.to_value()["error"]["code"], "invalid_arguments");
        assert!(missing.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("take a screenshot first"));

        let action =
            crate::adapters::translate("openai", &json!({"type": "move", "x": 100, "y": 50}))
                .unwrap();
        let adapted = dispatch_with_context(
            &backend,
            action,
            &CallContext {
                coord: Some("image".into()),
                session: Some("p5-map".into()),
                adapter: Some("openai".into()),
                ..CallContext::default()
            },
        );
        assert!(adapted.ok, "{adapted:?}");
        assert_eq!(adapted.to_value()["adapter"], "openai");
        assert_eq!(adapted.to_value()["x"], 176);
        assert_eq!(adapted.to_value()["y"], 68);
        assert!(adapted.to_value().get("state").is_none());

        let missing_adapter = handle_request_line_with(
            r#"{"adapter":"anthropic","action":"mouse_move","coordinate":[1,2],"session":"no-map"}"#,
            None,
        );
        assert!(!missing_adapter.ok);
        assert_eq!(
            missing_adapter.to_value()["error"]["code"],
            "invalid_arguments"
        );
        assert!(missing_adapter.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("take a screenshot first"));
        assert_eq!(missing_adapter.to_value()["adapter"], "anthropic");

        let waited = handle_request_line_with(r#"{"adapter":"openai","type":"wait"}"#, None);
        assert!(waited.ok, "{waited:?}");
        assert_eq!(waited.to_value()["adapter"], "openai");
        assert_eq!(waited.to_value()["waited_ms"], 0);
    }

    #[test]
    fn return_state_includes_the_numbered_tree_and_ignores_read_only() {
        let backend = StubBackend::default();
        let before = backend.accessibility_apps.lock().unwrap().len();
        let response = dispatch_with_context(
            &backend,
            Action::Click {
                x: Some(3),
                y: Some(4),
                button: MouseButton::Left,
                double: false,
                count: 1,
            },
            &CallContext {
                app: Some("Notes".into()),
                return_state: true,
                ..CallContext::default()
            },
        );
        assert!(response.ok, "{response:?}");
        let value = response.to_value();
        assert_eq!(value["action"], "click");
        let tree = value["state"]["tree"].as_str().unwrap();
        assert!(tree.contains("[0] AXWindow"), "{tree}");
        assert!(tree.contains("AXTextArea"), "{tree}");
        assert!(value["state"]["diff"].is_null());
        assert_eq!(value["state"]["app"], "com.apple.Notes");
        assert_eq!(value["state"]["pid"], 10);
        assert_eq!(value["state"]["window_id"], 100);
        assert!(value.get("warnings").is_none());
        assert!(backend.accessibility_apps.lock().unwrap().len() > before);

        let displays = dispatch_with_context(
            &backend,
            Action::Displays,
            &CallContext {
                return_state: true,
                include_screenshot: true,
                ..CallContext::default()
            },
        );
        assert!(displays.ok, "{displays:?}");
        assert!(displays.to_value().get("state").is_none());

        let cursor = dispatch_with(&backend, Action::CursorPosition);
        assert_eq!(cursor.to_value()["x"], 4);
        assert_eq!(cursor.to_value()["y"], 5);
        assert!(cursor.to_value().get("adapter").is_none());
    }

    #[test]
    fn return_state_refreshes_the_tree_diff_baseline() {
        let backend = StubBackend::default();
        let first = dispatch_with_context(&backend, Action::Tree, &ctx("Notes"));
        assert!(first.ok, "{first:?}");
        *backend.stub_value.lock().unwrap() = "ab".into();
        let mut context = element_context(0);
        context.return_state = true;
        let response = dispatch_with_context(&backend, Action::Focus, &context);
        assert!(response.ok, "{response:?}");
        let value = response.to_value();
        let diff = value["state"]["diff"].as_str().unwrap();
        assert!(
            diff.contains("~ [1] AXTextArea value: \"a\" -> \"ab\""),
            "{diff}"
        );
        assert!(value["state"]["tree"]
            .as_str()
            .unwrap()
            .contains("value=\"ab\""));
        *backend.stub_value.lock().unwrap() = "abc".into();
        let next = dispatch_with_context(&backend, Action::Tree, &ctx("Notes"));
        let value = next.to_value();
        let text = value["tree"].as_str().unwrap();
        assert!(
            text.contains("~ [1] AXTextArea value: \"ab\" -> \"abc\""),
            "{text}"
        );
        assert!(!text.contains("\"a\" -> \"abc\""), "{text}");
    }

    #[test]
    fn state_diff_omits_the_full_tree_and_keeps_the_delta() {
        let backend = StubBackend::default();
        let first = dispatch_with_context(&backend, Action::Tree, &ctx("Notes"));
        assert!(first.ok, "{first:?}");
        *backend.stub_value.lock().unwrap() = "ab".into();
        let mut context = element_context(0);
        context.return_state = true;
        context.state_diff = true;
        let response = dispatch_with_context(&backend, Action::Focus, &context);
        assert!(response.ok, "{response:?}");
        let value = response.to_value();
        // The full tree is the expensive half of the payload. It is gone.
        assert!(value["state"].get("tree").is_none(), "{value}");
        let diff = value["state"]["diff"].as_str().unwrap();
        assert!(
            diff.contains("~ [1] AXTextArea value: \"a\" -> \"ab\""),
            "{diff}"
        );
        assert_eq!(value["state"]["baseline"], "existing");
        assert_eq!(value["state"]["app"], "com.apple.Notes");
    }

    #[test]
    fn state_diff_without_state_still_attaches_the_post_state() {
        let backend = StubBackend::default();
        let seeded = dispatch_with_context(&backend, Action::Tree, &ctx("Notes"));
        assert!(seeded.ok, "{seeded:?}");
        let mut context = element_context(0);
        context.state_diff = true;
        let response = dispatch_with_context(&backend, Action::Focus, &context);
        assert!(response.ok, "{response:?}");
        let value = response.to_value();
        assert!(value["state"].get("tree").is_none(), "{value}");
        assert!(value["state"].get("diff").is_some(), "{value}");
    }

    #[test]
    fn the_tree_response_says_whether_a_baseline_existed() {
        let backend = StubBackend::default();
        let first = dispatch_with_context(&backend, Action::Tree, &ctx("Notes"));
        assert!(first.ok, "{first:?}");
        assert_eq!(first.to_value()["baseline"], "created");
        *backend.stub_value.lock().unwrap() = "ab".into();
        let second = dispatch_with_context(&backend, Action::Tree, &ctx("Notes"));
        assert!(second.ok, "{second:?}");
        assert_eq!(second.to_value()["baseline"], "existing");
    }

    #[test]
    fn policy_check_reports_a_code_next_to_the_decision() {
        let backend = StubBackend::default();
        let response = dispatch_with_context(
            &backend,
            Action::PolicyCheck,
            &CallContext {
                app: Some("Notes".into()),
                ..CallContext::default()
            },
        );
        assert!(response.ok, "{response:?}");
        let value = response.to_value();
        assert_eq!(value["decision"], "allow");
        assert_eq!(value["code"], "ok");
        assert!(value["source"].is_string(), "{value}");
    }

    #[test]
    fn include_screenshot_failure_keeps_the_action_result() {
        let backend = StubBackend {
            capture_fault: crate::test_backend::StubCaptureFault::Permission,
            ..StubBackend::default()
        };
        let response = dispatch_with_context(
            &backend,
            Action::Click {
                x: Some(1),
                y: Some(2),
                button: MouseButton::Left,
                double: false,
                count: 1,
            },
            &CallContext {
                app: Some("Notes".into()),
                return_state: true,
                include_screenshot: true,
                ..CallContext::default()
            },
        );
        assert!(response.ok, "{response:?}");
        let value = response.to_value();
        assert_eq!(value["action"], "click");
        assert_eq!(value["x"], 1);
        assert_eq!(value["y"], 2);
        assert!(value["state"]["tree"]
            .as_str()
            .unwrap()
            .contains("AXWindow"));
        assert!(value["state"]["screenshot"].is_null());
        let warnings = value["warnings"].as_array().unwrap();
        assert!(warnings
            .iter()
            .any(|item| { item.as_str().unwrap().contains("screenshot failed") }));
        assert_eq!(backend.clicks.load(Ordering::SeqCst), 1);
    }

    fn batch_clicks(stop_on_error: bool) -> Value {
        json!({
            "action": "batch",
            "stop_on_error": stop_on_error,
            "actions": [
                {"action": "click", "x": 1, "y": 2},
                {"action": "click", "x": 3, "y": 4}
            ]
        })
    }

    #[test]
    fn batch_stop_on_error_controls_the_second_attempt() {
        let stopped = StubBackend {
            fail_click: true,
            ..StubBackend::default()
        };
        let request = parse_request(batch_clicks(true)).unwrap();
        let response = dispatch_with_context(&stopped, request.action, &request.context);
        assert!(!response.ok);
        let value = response.to_value();
        assert_eq!(value["error"]["code"], "execution_failed");
        assert_eq!(value["stopped_at"], 0);
        assert_eq!(value["results"].as_array().unwrap().len(), 1);
        assert!(!value["results"][0]["ok"].as_bool().unwrap());
        assert_eq!(stopped.click_attempts.load(Ordering::SeqCst), 2);
        assert!(value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("batch failed at action 0"));

        let both = StubBackend {
            fail_click: true,
            ..StubBackend::default()
        };
        let request = parse_request(batch_clicks(false)).unwrap();
        let response = dispatch_with_context(&both, request.action, &request.context);
        assert!(!response.ok);
        let value = response.to_value();
        assert!(value.get("stopped_at").is_none(), "{value}");
        assert_eq!(value["results"].as_array().unwrap().len(), 2);
        assert!(!value["results"][1]["ok"].as_bool().unwrap());
        assert_eq!(both.click_attempts.load(Ordering::SeqCst), 4);

        let nested = parse_request(json!({
            "action": "batch",
            "actions": [{"action": "batch", "actions": []}]
        }))
        .unwrap();
        let response = dispatch_with(&StubBackend::default(), nested.action);
        assert_eq!(response.to_value()["error"]["code"], "invalid_arguments");
        assert_eq!(response.to_value()["stopped_at"], 0);
        assert!(response.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("nested batch"));

        let backend = StubBackend::default();
        backend.seed_element(notes_button());
        let request = parse_request(json!({
            "action": "batch",
            "app": "Notes",
            "actions": [
                {"action": "press", "element": 1},
                {"action": "focus", "element": 1}
            ]
        }))
        .unwrap();
        assert!(matches!(
            request.action,
            Action::Batch {
                stop_on_error: true,
                ..
            }
        ));
        let response = dispatch_with_context(&backend, request.action, &request.context);
        assert!(response.ok, "{response:?}");
        let results = response.to_value()["results"].clone();
        assert_eq!(results[0]["action"], "press");
        assert_eq!(results[0]["element"], 1);
        assert_eq!(results[1]["action"], "focus");
        assert!(response.to_value().get("state").is_none());
    }

    #[test]
    fn p9_menu_list_round_trip_records_no_args() {
        let backend = StubBackend::default();
        backend
            .menu_sections
            .lock()
            .unwrap()
            .push(crate::backend::MenuNode {
                title: "File".into(),
                enabled: true,
                children: vec![crate::backend::MenuNode {
                    title: "Save".into(),
                    enabled: true,
                    children: Vec::new(),
                }],
            });
        let response = dispatch_with_context(
            &backend,
            Action::MenuList {
                app: Some("Notes".into()),
                depth: Some(2),
            },
            &ctx("Notes"),
        );
        assert!(response.ok, "{:?}", response.to_value());
        let value = response.to_value();
        assert_eq!(value["action"], "menu_list");
        assert_eq!(value["app"], "Notes");
        assert_eq!(value["sections"][0]["title"], "File");
        assert_eq!(value["sections"][0]["children"][0]["title"], "Save");
    }

    #[test]
    fn p9_menu_click_records_path_and_reports_disabled() {
        let mut backend = StubBackend::default();
        let response = dispatch_with_context(
            &backend,
            Action::MenuClick {
                app: Some("Notes".into()),
                path: "File > Save".into(),
            },
            &ctx("Notes"),
        );
        assert!(response.ok, "{:?}", response.to_value());
        let value = response.to_value();
        assert_eq!(value["path"], serde_json::json!(["File", "Save"]));
        assert_eq!(value["pressed"], true);
        let recorded = backend.menu_clicks.lock().unwrap().clone();
        assert_eq!(recorded.len(), 1);
        assert_eq!(
            recorded[0].segments,
            vec!["File".to_string(), "Save".to_string()]
        );

        backend.menu_disabled = true;
        let response = dispatch_with_context(
            &backend,
            Action::MenuClick {
                app: Some("Notes".into()),
                path: "File > Save".into(),
            },
            &ctx("Notes"),
        );
        assert!(!response.ok, "disabled menu item must fail");
        assert_eq!(response.to_value()["error"]["code"], "execution_failed");
    }

    #[test]
    fn p9_apple_event_records_only_length_and_language() {
        let backend = StubBackend::default();
        *backend.apple_event_stdout.lock().unwrap() = "Notes".into();
        let secret = "system prompt with token ABCDEFG";
        let (allow, dir) = allow_confirm_hook();
        let mut context = ctx("Notes");
        context.confirm_cmd = Some(allow);
        let response = dispatch_with_context(
            &backend,
            Action::AppleEvent {
                app: Some("Notes".into()),
                script: secret.into(),
                language: Some(AppleEventLanguage::AppleScript),
            },
            &context,
        );
        assert!(response.ok, "{:?}", response.to_value());
        let value = response.to_value();
        assert_eq!(value["language"], "AppleScript");
        assert_eq!(value["script_bytes"], secret.len());
        assert_eq!(value["stdout"], "Notes");
        assert!(value["script_hash"].is_string());
        let recorded = backend.apple_event_calls.lock().unwrap().clone();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].1, AppleEventLanguage::AppleScript);
        // Audit-style storage must not capture the body verbatim.
        assert_eq!(recorded[0].2, secret.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn p9_apple_event_records_language_javascript() {
        let backend = StubBackend::default();
        let (allow, dir) = allow_confirm_hook();
        let mut context = ctx("Notes");
        context.confirm_cmd = Some(allow);
        let response = dispatch_with_context(
            &backend,
            Action::AppleEvent {
                app: Some("Notes".into()),
                script: "Application('Notes').name()".into(),
                language: Some(AppleEventLanguage::JavaScript),
            },
            &context,
        );
        assert!(response.ok);
        assert_eq!(response.to_value()["language"], "JavaScript");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn p9_apple_event_failure_surfaces_execution_failed() {
        let backend = StubBackend {
            apple_event_fail: true,
            ..Default::default()
        };
        *backend.apple_event_stderr.lock().unwrap() = "execution error: 42".into();
        let (allow, dir) = allow_confirm_hook();
        let mut context = ctx("Notes");
        context.confirm_cmd = Some(allow);
        let response = dispatch_with_context(
            &backend,
            Action::AppleEvent {
                app: Some("Notes".into()),
                script: "1/0".into(),
                language: None,
            },
            &context,
        );
        assert!(!response.ok);
        assert_eq!(response.to_value()["error"]["code"], "execution_failed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn p9_dock_list_and_click_round_trip() {
        let backend = StubBackend::default();
        backend.dock_items.lock().unwrap().extend([
            "Finder".to_string(),
            "Terminal".to_string(),
            "Mail".to_string(),
        ]);
        let response = dispatch_with(&backend, Action::DockList {});
        assert!(response.ok);
        let value = response.to_value();
        let items = value["items"].as_array().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["title"], "Finder");

        let response = dispatch_with(
            &backend,
            Action::DockClick {
                title: "terminal".into(),
            },
        );
        assert!(response.ok, "{:?}", response.to_value());
        assert_eq!(response.to_value()["pressed"], true);
        assert_eq!(
            backend.dock_clicks.lock().unwrap().clone(),
            vec!["terminal".to_string()]
        );

        let response = dispatch_with(
            &backend,
            Action::DockClick {
                title: "missing".into(),
            },
        );
        assert!(!response.ok);
        assert_eq!(response.to_value()["error"]["code"], "element_not_found");
    }

    #[test]
    fn p9_spaces_reports_screen_groups_from_fake_list() {
        use crate::target::{WindowBounds, WindowInfo};
        use crate::types::DisplayInfo;
        use crate::window_groups::SpacesReport;

        let mut spaces_report = SpacesReport {
            screens: Vec::new(),
            note: String::new(),
        };
        spaces_report
            .screens
            .push(crate::window_groups::SpaceScreen {
                index: 0,
                name: "Display 0".into(),
                x: 0,
                y: 0,
                width: 1000,
                height: 1000,
                is_primary: true,
                windows: vec![WindowInfo {
                    id: 1,
                    pid: 100,
                    app: "Notes".into(),
                    title: "Note 1".into(),
                    bounds: WindowBounds {
                        x: 10,
                        y: 10,
                        width: 200,
                        height: 200,
                    },
                    z: 0,
                    layer: 0,
                    on_screen: true,
                    ax_matched: false,
                }],
            });

        let backend = StubBackend::default();
        *backend.spaces_report.lock().unwrap() = spaces_report;
        let response = dispatch_with(&backend, Action::Spaces {});
        assert!(response.ok);
        let value = response.to_value();
        assert_eq!(value["action"], "spaces");
        assert_eq!(value["screens"][0]["name"], "Display 0");
        assert_eq!(value["screens"][0]["windows"][0]["title"], "Note 1");
        assert_eq!(value["screens"][0]["is_primary"], true);
        // `group_windows` itself is exercised in window_groups tests.
        let _ = DisplayInfo {
            id: 0,
            name: String::new(),
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            scale: 1.0,
            is_primary: false,
        };
    }

    #[test]
    fn p9_shortcuts_run_invokes_with_only_the_name() {
        // The test backend does not exercise the binary; this test only
        // confirms dispatch reaches the helper when the binary is missing.
        let backend = StubBackend::default();
        let response = dispatch_with(
            &backend,
            Action::ShortcutsRun {
                name: "Open URL".into(),
            },
        );
        // On macOS the dispatch path reaches the helper, so the response is
        // either ok (binary present) or `execution_failed` (binary absent).
        // Off macOS the dispatcher short-circuits to `unsupported`. Either
        // outcome is acceptable; the failure code must be the documented
        // one.
        if !response.ok {
            let value = response.to_value();
            let code = value["error"]["code"].as_str().unwrap_or_default();
            assert!(
                matches!(
                    code,
                    "execution_failed" | "unsupported" | "confirm_required"
                ),
                "{value:?}"
            );
        }
    }

    #[test]
    fn session_prune_action_removes_an_empty_temp_session() {
        let _cache = crate::cache::CacheEnv::acquire();
        let dir = cache::session_dir(Some("empty-session")).unwrap();
        assert!(dir.join("owner.json").is_file());
        let response = dispatch(Action::SessionPrune {
            dry_run: true,
            ttl_secs: Some(0),
        });
        assert!(response.ok, "{response:?}");
        assert_eq!(response.to_value()["dry_run"], true);
        assert!(response.to_value()["removed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "empty-session"));
        assert!(dir.exists());
        let response = dispatch(Action::SessionPrune {
            dry_run: false,
            ttl_secs: Some(0),
        });
        assert!(response.ok, "{response:?}");
        assert!(!dir.exists());
    }

    #[test]
    fn pause_short_circuits_before_the_stub_clicks() {
        let backend = StubBackend::default();
        let _paused = policy::override_paused(true);
        let response = dispatch_with(
            &backend,
            Action::Click {
                x: Some(1),
                y: Some(2),
                button: MouseButton::Left,
                double: false,
                count: 1,
            },
        );
        assert_eq!(response.to_value()["error"]["code"], "paused");
        assert_eq!(backend.clicks.load(Ordering::SeqCst), 0);
        assert_eq!(backend.click_attempts.load(Ordering::SeqCst), 0);
        let waited = dispatch_with(&backend, Action::wait_sleep(Some(0), None));
        assert!(waited.ok, "{waited:?}");
    }

    #[test]
    fn confirm_hook_allows_quit_and_a_failure_does_not() {
        let backend = StubBackend::default();
        let dir = std::env::temp_dir().join(format!(
            "computer-use-hook-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        let (allow, deny) = {
            let allow = dir.join("allow.sh");
            let deny = dir.join("deny.sh");
            std::fs::write(&allow, "#!/bin/sh\ncat >/dev/null\nexit 0\n").unwrap();
            std::fs::write(&deny, "#!/bin/sh\ncat >/dev/null\nexit 1\n").unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&allow, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::set_permissions(&deny, std::fs::Permissions::from_mode(0o700)).unwrap();
            (allow.display().to_string(), deny.display().to_string())
        };
        #[cfg(windows)]
        let (allow, deny) = ("cmd /C exit 0".to_string(), "cmd /C exit 1".to_string());
        let mut context = ctx("412");
        context.confirm_cmd = Some(deny);
        let denied = dispatch_with_context(&backend, Action::Quit { force: false }, &context);
        assert_eq!(denied.to_value()["error"]["code"], "confirm_denied");
        assert!(backend.quit.lock().unwrap().is_empty());
        context.confirm_cmd = Some(allow);
        let allowed = dispatch_with_context(&backend, Action::Quit { force: false }, &context);
        assert!(allowed.ok, "{allowed:?}");
        assert_eq!(backend.quit.lock().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn blocked_url_prevents_the_click() {
        let mut backend = StubBackend::default();
        backend.apps.push(crate::target::AppInfo {
            name: "Safari".into(),
            bundle_id: Some("com.apple.Safari".into()),
            pid: 80,
            path: Some("/Applications/Safari.app/Contents/MacOS/Safari".into()),
            active: false,
            hidden: false,
            activation_policy: crate::target::ActivationPolicy::Regular,
        });
        backend
            .browser_urls
            .lock()
            .unwrap()
            .insert(80, "https://accounts.example.com/login".into());
        let dir = std::env::temp_dir().join(format!(
            "computer-use-url-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let policy_path = dir.join("policy.toml");
        std::fs::write(
            &policy_path,
            "version = 1\n[browser]\nblocked_urls = [\"https://accounts.example.com/*\"]\n",
        )
        .unwrap();
        let mut context = ctx("80");
        context.policy = Some(policy_path.display().to_string());
        let response = dispatch_with_context(
            &backend,
            Action::Click {
                x: Some(3),
                y: Some(4),
                button: MouseButton::Left,
                double: false,
                count: 1,
            },
            &context,
        );
        assert_eq!(response.to_value()["error"]["code"], "blocked_url");
        assert!(response.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("https://accounts.example.com/*"));
        assert!(!response.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("login"));
        assert_eq!(backend.click_attempts.load(Ordering::SeqCst), 0);
        backend
            .browser_urls
            .lock()
            .unwrap()
            .insert(80, "https://example.com/".into());
        let allowed = dispatch_with_context(
            &backend,
            Action::Click {
                x: Some(3),
                y: Some(4),
                button: MouseButton::Left,
                double: false,
                count: 1,
            },
            &context,
        );
        assert!(allowed.ok, "{allowed:?}");
        assert_eq!(backend.clicks.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "computer-use-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn click_at(x: i32, y: i32) -> Action {
        Action::Click {
            x: Some(x),
            y: Some(y),
            button: MouseButton::Left,
            double: false,
            count: 1,
        }
    }

    fn last_audit(dir: &std::path::Path) -> Value {
        let text = std::fs::read_to_string(dir.join("audit.jsonl")).unwrap();
        serde_json::from_str(text.lines().last().unwrap()).unwrap()
    }

    #[test]
    fn auto_event_falls_back_to_activate_unless_blocked() {
        let backend = StubBackend {
            fail_tiers: std::sync::Mutex::new(vec!["event".into()]),
            ..StubBackend::default()
        };
        let response = dispatch_with(&backend, click_at(3, 4));
        assert!(response.ok, "{response:?}");
        let value = response.to_value();
        assert_eq!(value["tier"], "activate");
        assert_eq!(value["cursor_moved"], true);
        assert_eq!(value["focus_restored"], true);
        assert_eq!(
            backend.tiers_run.lock().unwrap().as_slice(),
            ["event", "activate"]
        );
        assert_eq!(backend.clicks.load(Ordering::SeqCst), 1);

        let held = StubBackend {
            fail_tiers: std::sync::Mutex::new(vec!["event".into()]),
            ..StubBackend::default()
        };
        let dir = temp_dir("lock-fallback");
        let path = dir.join("foreground.lock");
        let _slot = crate::foreground::push_lock_path(&path);
        let _lock = crate::foreground::try_lock(&path).unwrap();
        let response = dispatch_with(&held, click_at(3, 4));
        assert_eq!(response.to_value()["error"]["code"], "execution_failed");
        assert!(response.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("stub tier event failed"));
        assert_eq!(held.tiers_run.lock().unwrap().as_slice(), ["event"]);
        assert_eq!(held.clicks.load(Ordering::SeqCst), 0);
        drop(_lock);
        let _ = std::fs::remove_dir_all(&dir);

        let background = StubBackend {
            fail_tiers: std::sync::Mutex::new(vec!["event".into()]),
            ..StubBackend::default()
        };
        let response = dispatch_with_context(
            &background,
            click_at(1, 2),
            &CallContext {
                require_background: true,
                ..CallContext::default()
            },
        );
        assert_eq!(response.to_value()["error"]["code"], "execution_failed");
        assert_eq!(background.tiers_run.lock().unwrap().as_slice(), ["event"]);
        assert_eq!(background.clicks.load(Ordering::SeqCst), 0);

        let refused = StubBackend::default();
        let response = dispatch_with_context(
            &refused,
            click_at(1, 2),
            &CallContext {
                tier: Some("activate".into()),
                require_background: true,
                ..CallContext::default()
            },
        );
        assert!(response.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--require-background"));
        assert!(refused.tiers_run.lock().unwrap().is_empty());
        assert_eq!(refused.clicks.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn activate_names_the_lock_holder_before_it_clicks() {
        let backend = StubBackend::default();
        let dir = temp_dir("lock-held");
        let path = dir.join("foreground.lock");
        let _slot = crate::foreground::push_lock_path(&path);
        let _lock = crate::foreground::try_lock(&path).unwrap();
        let response = dispatch_with_context(
            &backend,
            click_at(9, 8),
            &CallContext {
                tier: Some("activate".into()),
                ..CallContext::default()
            },
        );
        assert_eq!(response.to_value()["error"]["code"], "execution_failed");
        let message = response.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            message.contains(&format!(
                "foreground.lock is held by pid {}",
                std::process::id()
            )),
            "{message}"
        );
        assert_eq!(backend.clicks.load(Ordering::SeqCst), 0);
        assert_eq!(backend.hit_calls.load(Ordering::SeqCst), 1);
        assert!(backend.tiers_run.lock().unwrap().is_empty());
        drop(_lock);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn synthetic_without_the_feature_does_not_hit_test_or_click() {
        let backend = StubBackend::default();
        let response = dispatch_with_context(
            &backend,
            click_at(1, 1),
            &CallContext {
                tier: Some("synthetic".into()),
                ..CallContext::default()
            },
        );
        if crate::tier::skylight_compiled() {
            assert!(response.ok, "{response:?}");
            assert_eq!(response.to_value()["tier"], "synthetic");
            assert_eq!(backend.clicks.load(Ordering::SeqCst), 1);
            assert_eq!(backend.tiers_run.lock().unwrap().as_slice(), ["synthetic"]);
        } else {
            assert_eq!(response.to_value()["error"]["code"], "unsupported");
            assert!(response.to_value()["error"]["message"]
                .as_str()
                .unwrap()
                .contains("built without skylight feature"));
            assert_eq!(backend.hit_calls.load(Ordering::SeqCst), 0);
            assert_eq!(backend.clicks.load(Ordering::SeqCst), 0);
            assert!(backend.tiers_run.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn missing_overlay_helper_warns_and_no_overlay_skips_it() {
        let dir = temp_dir("overlay-warn");
        let _slot = crate::overlay::push_socket_path(dir.join("missing.sock"));
        let backend = StubBackend::default();
        let response = dispatch_with_context(
            &backend,
            click_at(4, 5),
            &CallContext {
                session: Some("agent".into()),
                ..CallContext::default()
            },
        );
        assert!(response.ok, "{response:?}");
        assert_eq!(response.to_value()["tier"], "event");
        assert_eq!(response.to_value()["cursor_moved"], false);
        assert!(response.to_value().get("focus_restored").is_none());
        let warnings = response.to_value()["warnings"].as_array().unwrap().clone();
        assert!(warnings
            .iter()
            .any(|item| item == "overlay helper not running"));

        let quiet = dispatch_with_context(
            &backend,
            click_at(4, 5),
            &CallContext {
                no_overlay: true,
                ..CallContext::default()
            },
        );
        assert!(quiet.ok, "{quiet:?}");
        assert!(quiet.to_value().get("warnings").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn audit_records_decision_source_and_the_real_code() {
        let dir = temp_dir("audit");
        let _state = crate::audit::push_state_dir(&dir);
        let backend = StubBackend::default();
        let response = dispatch_with(&backend, click_at(2, 3));
        assert!(response.ok, "{response:?}");
        let line = last_audit(&dir);
        assert_eq!(line["action"], "click");
        assert_eq!(line["code"], "ok");
        assert_eq!(line["decision"], "allow");
        assert_eq!(line["source"], "default");
        assert_eq!(line["tier"], "event");
        assert!(line.get("text").is_none());

        let denied = dispatch_with_context(&backend, Action::Launch, &ctx("com.apple.Terminal"));
        assert_eq!(denied.to_value()["error"]["code"], "policy_denied");
        let line = last_audit(&dir);
        assert_eq!(line["action"], "launch");
        assert_eq!(line["code"], "policy_denied");
        assert_eq!(line["decision"], "deny");
        assert_eq!(line["source"], "builtin");

        let checked =
            dispatch_with_context(&backend, Action::PolicyCheck, &ctx("com.apple.Terminal"));
        assert!(checked.ok, "{checked:?}");
        assert_eq!(checked.to_value()["decision"], "deny");
        let line = last_audit(&dir);
        assert_eq!(line["action"], "policy_check");
        assert_eq!(line["code"], "ok");
        assert_eq!(line["decision"], "deny");
        assert_eq!(line["source"], "builtin");

        let bad = dispatch_with(
            &backend,
            Action::Click {
                x: None,
                y: None,
                button: MouseButton::Left,
                double: false,
                count: 1,
            },
        );
        assert_eq!(bad.to_value()["error"]["code"], "invalid_arguments");
        let line = last_audit(&dir);
        assert_eq!(line["code"], "invalid_arguments");
        assert_eq!(line["decision"], "allow");
        assert_eq!(line["source"], "default");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn drag_hold_click_and_keys_use_the_stub() {
        let backend = StubBackend::default();
        let short = dispatch_with(
            &backend,
            Action::Drag {
                path: vec![crate::types::Point { x: 1, y: 1 }],
                button: MouseButton::Left,
            },
        );
        assert_eq!(short.to_value()["error"]["code"], "invalid_arguments");
        assert_eq!(backend.moves.load(Ordering::SeqCst), 0);

        let dragged = dispatch_with(
            &backend,
            Action::Drag {
                path: vec![
                    crate::types::Point { x: 1, y: 2 },
                    crate::types::Point { x: 3, y: 4 },
                    crate::types::Point { x: 5, y: 6 },
                ],
                button: MouseButton::Left,
            },
        );
        assert!(dragged.ok, "{dragged:?}");
        assert_eq!(dragged.to_value()["moves"], 2);
        assert_eq!(backend.moves.load(Ordering::SeqCst), 2);
        assert_eq!(dragged.to_value()["tier"], "event");

        let ax = dispatch_with_context(
            &backend,
            Action::Drag {
                path: vec![
                    crate::types::Point { x: 1, y: 2 },
                    crate::types::Point { x: 3, y: 4 },
                ],
                button: MouseButton::Left,
            },
            &CallContext {
                tier: Some("ax".into()),
                ..CallContext::default()
            },
        );
        assert_eq!(ax.to_value()["error"]["code"], "unsupported");
        assert!(ax.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("pass --element"));
        assert_eq!(backend.moves.load(Ordering::SeqCst), 2);

        let triple = dispatch_with(
            &backend,
            Action::TripleClick {
                x: Some(8),
                y: Some(9),
                button: MouseButton::Left,
            },
        );
        assert!(triple.ok, "{triple:?}");
        assert_eq!(triple.to_value()["action"], "triple_click");
        assert_eq!(triple.to_value()["count"], 3);
        assert_eq!(backend.clicks.load(Ordering::SeqCst), 3);

        let held = dispatch_with(
            &backend,
            Action::HoldKey {
                keys: "shift".into(),
                ms: 40_000,
            },
        );
        assert_eq!(held.to_value()["error"]["code"], "invalid_arguments");
        assert!(backend.holds_ms.lock().unwrap().is_empty());

        let shift = dispatch_with(
            &backend,
            Action::HoldKey {
                keys: "shift".into(),
                ms: 400,
            },
        );
        assert!(shift.ok, "{shift:?}");
        assert_eq!(backend.holds_ms.lock().unwrap().as_slice(), [400]);

        let sequence = dispatch_with(
            &backend,
            Action::Key {
                keys: "super+c Return".into(),
            },
        );
        assert!(sequence.ok, "{sequence:?}");
        assert_eq!(sequence.to_value()["strokes"], 2);
        let keys = backend.keys.lock().unwrap();
        let sequence = &keys[keys.len() - 2..];
        assert_eq!(sequence[0].1, "c");
        assert_eq!(sequence[0].0, vec![Modifier::Meta]);
        assert_eq!(sequence[1].1, "enter");
        drop(keys);

        let unknown = dispatch_with(
            &backend,
            Action::Key {
                keys: "bogus".into(),
            },
        );
        assert_eq!(unknown.to_value()["error"]["code"], "invalid_arguments");
    }

    #[test]
    fn mouse_down_releases_when_the_session_ends() {
        let backend = StubBackend::default();
        let down = Action::MouseDown {
            x: 4,
            y: 5,
            button: MouseButton::Left,
        };
        let alone = dispatch_with(&backend, down.clone());
        assert!(alone.ok, "{alone:?}");
        assert!(backend.held_buttons.lock().unwrap().is_empty());
        assert_eq!(
            backend.releases.lock().unwrap().as_slice(),
            [MouseButton::Left]
        );

        backend.releases.lock().unwrap().clear();
        let _session = crate::InputSession::open(&backend);
        let kept = dispatch_with(&backend, down);
        assert!(kept.ok, "{kept:?}");
        assert_eq!(
            backend.held_buttons.lock().unwrap().as_slice(),
            [MouseButton::Left]
        );
        assert!(backend.releases.lock().unwrap().is_empty());
        drop(_session);
        assert!(backend.held_buttons.lock().unwrap().is_empty());
        assert_eq!(
            backend.releases.lock().unwrap().as_slice(),
            [MouseButton::Left]
        );

        let batch = dispatch_with(
            &backend,
            Action::Batch {
                stop_on_error: true,
                actions: vec![
                    json!({"action": "mouse_down", "x": 1, "y": 2}),
                    json!({"action": "mouse_up", "x": 1, "y": 2}),
                ],
            },
        );
        assert!(batch.ok, "{batch:?}");
        assert!(backend.held_buttons.lock().unwrap().is_empty());
    }

    #[test]
    fn clipboard_confirms_and_omits_the_text_from_the_audit() {
        let dir = temp_dir("clipboard-audit");
        let _state = crate::audit::push_state_dir(&dir);
        let _cache = crate::cache::CacheEnv::acquire();
        let backend = StubBackend::default();
        let secret = "do-not-log";
        let response = dispatch_with(
            &backend,
            Action::ClipboardSet {
                text: Some(secret.into()),
                image: None,
            },
        );
        assert_eq!(response.to_value()["error"]["code"], "confirm_required");
        assert!(backend.clipboard_text.lock().unwrap().is_none());
        let line = last_audit(&dir);
        assert_eq!(line["action"], "clipboard_set");
        assert_eq!(line["decision"], "confirm");
        assert_eq!(line["chars"], secret.chars().count() as u64);
        let encoded = line.to_string();
        assert!(!encoded.contains(secret));
        assert!(line.get("text").is_none());

        *backend.clipboard_text.lock().unwrap() = Some("ready".into());
        let got = dispatch_with(&backend, Action::ClipboardGet);
        assert!(got.ok, "{got:?}");
        assert_eq!(got.to_value()["kind"], "text");
        assert_eq!(got.to_value()["text"], "ready");

        let empty = dispatch_with(
            &backend,
            Action::ClipboardSet {
                text: Some(String::new()),
                image: None,
            },
        );
        assert_eq!(empty.to_value()["error"]["code"], "invalid_arguments");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zoom_crops_a_window_region_and_rejects_the_far_edge() {
        let _cache = crate::cache::CacheEnv::acquire();
        let backend = StubBackend::default();
        let response = dispatch_with_context(
            &backend,
            Action::Zoom {
                output: None,
                region: Region::new(0, 0, 10, 10).unwrap(),
                format: Some(ImageFormat::Png),
                max_width: Some(0),
                canvas: None,
                base64: false,
            },
            &CallContext {
                app: Some("Notes".into()),
                session: Some("p8-zoom".into()),
                ..CallContext::default()
            },
        );
        assert!(response.ok, "{response:?}");
        let value = response.to_value();
        assert_eq!(value["width"], 20);
        assert_eq!(value["height"], 20);
        assert_eq!(value["logical_width"], 10);
        assert_eq!(value["logical_height"], 10);

        let outside = dispatch_with_context(
            &backend,
            Action::Zoom {
                output: None,
                region: Region::new(0, 0, 401, 300).unwrap(),
                format: None,
                max_width: Some(0),
                canvas: None,
                base64: false,
            },
            &CallContext {
                app: Some("Notes".into()),
                session: Some("p8-zoom-out".into()),
                ..CallContext::default()
            },
        );
        assert_eq!(outside.to_value()["error"]["code"], "invalid_arguments");

        let both = dispatch_with_context(
            &backend,
            Action::Screenshot {
                output: None,
                region: Some(Region::new(0, 0, 10, 10).unwrap()),
                format: None,
                max_width: None,
                canvas: None,
                base64: false,
                marks: false,
            },
            &CallContext {
                app: Some("Notes".into()),
                ..CallContext::default()
            },
        );
        assert_eq!(both.to_value()["error"]["code"], "invalid_arguments");
        assert!(both.to_value()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not both"));
    }

    // ----- P10 gap fixes -----

    #[test]
    fn browser_text_action_round_trips_with_only_selector() {
        let text: Action =
            serde_json::from_str(r#"{"action":"browser_text","selector":"h1"}"#).unwrap();
        match &text {
            Action::BrowserText {
                selector,
                target_id,
                ref_id,
            } => {
                assert_eq!(selector.as_deref(), Some("h1"));
                assert!(target_id.is_none());
                assert!(ref_id.is_none());
            }
            other => panic!("unexpected {other:?}"),
        }
        let encoded = serde_json::to_string(&text).unwrap();
        let decoded: Action = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, text);
    }

    #[test]
    fn browser_type_action_round_trips_with_text_and_optional_selector() {
        let action: Action =
            serde_json::from_str(r##"{"action":"browser_type","text":"hi","selector":"#name"}"##)
                .unwrap();
        match &action {
            Action::BrowserType {
                text,
                selector,
                target_id,
                ref_id,
                clear,
                submit,
                wait,
                timeout_ms,
            } => {
                assert_eq!(text, "hi");
                assert_eq!(selector.as_deref(), Some("#name"));
                assert!(target_id.is_none());
                assert!(ref_id.is_none());
                assert!(!clear);
                assert!(!submit);
                assert!(wait.is_none());
                assert!(timeout_ms.is_none());
            }
            other => panic!("unexpected {other:?}"),
        }
        let encoded = serde_json::to_string(&action).unwrap();
        let decoded: Action = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, action);
    }

    #[test]
    fn browser_text_label_is_browser_text_and_class_is_observe() {
        // BrowserText reads innerText; classified Observe (read-only).
        let action = Action::BrowserText {
            selector: Some("h1".into()),
            ref_id: None,
            target_id: None,
        };
        assert_eq!(action_label(&action), "browser_text");
        assert!(matches!(policy_class(&action), PolicyClass::Observe));
    }

    #[test]
    fn browser_type_label_is_browser_type_and_class_is_mutate() {
        // BrowserType types text; classified Mutate (write). The text body
        // never leaves the audit row — only the char count does.
        let action = Action::BrowserType {
            text: "abc".into(),
            selector: Some("#name".into()),
            ref_id: None,
            target_id: None,
            clear: false,
            submit: false,
            wait: None,
            timeout_ms: None,
        };
        assert_eq!(action_label(&action), "browser_type");
        assert!(matches!(policy_class(&action), PolicyClass::Mutate));
        assert_eq!(clipboard_chars(&action), Some(3));
    }

    #[test]
    fn browser_text_does_not_record_chars_in_audit() {
        // Reading innerText must not push a chars count into the audit row.
        let action = Action::BrowserText {
            selector: Some("h1".into()),
            ref_id: None,
            target_id: None,
        };
        assert_eq!(clipboard_chars(&action), None);
    }

    #[test]
    #[cfg(feature = "cdp")]
    fn browser_navigate_blocks_url_before_connecting() {
        // Use a deliberately unreachable WebSocket URL. If the URL block ran
        // before the connect attempt, we get blocked_url. If the order were
        // wrong we would see cdp_unavailable from the connection failure.
        let backend = StubBackend::default();
        let context = CallContext {
            cdp: true,
            cdp_url: Some("ws://127.0.0.1:1".into()),
            browser_blocked_urls: vec!["https://blocked.example.com".into()],
            ..CallContext::default()
        };
        let response = dispatch_with_context(
            &backend,
            Action::BrowserNavigate {
                url: "https://blocked.example.com".into(),
                target_id: None,
                wait: None,
                timeout_ms: None,
                new_tab: false,
            },
            &context,
        );
        assert!(!response.ok);
        let value = response.to_value();
        assert_eq!(value["error"]["code"], "blocked_url");
    }

    #[test]
    #[cfg(feature = "cdp")]
    fn browser_navigate_unblocked_url_still_attempts_connect() {
        // Sanity: without a matching pattern, navigate goes through the
        // normal path and reports the connect failure (cdp_unavailable).
        let backend = StubBackend::default();
        let context = CallContext {
            cdp: true,
            cdp_url: Some("ws://127.0.0.1:1".into()),
            ..CallContext::default()
        };
        let response = dispatch_with_context(
            &backend,
            Action::BrowserNavigate {
                url: "https://example.com".into(),
                target_id: None,
                wait: None,
                timeout_ms: None,
                new_tab: false,
            },
            &context,
        );
        assert!(!response.ok);
        let value = response.to_value();
        assert_eq!(value["error"]["code"], "cdp_unavailable");
    }

    // -----------------------------------------------------------------------
    // P14: the `--display` filter.
    // -----------------------------------------------------------------------

    fn filtered(spec: &str) -> CallContext {
        CallContext {
            display: Some(spec.into()),
            ..CallContext::default()
        }
    }

    fn code_of(response: &Response) -> String {
        response.to_value()["error"]["code"]
            .as_str()
            .unwrap_or("")
            .to_string()
    }

    #[test]
    fn display_filter_rejects_a_point_outside_the_chosen_monitor() {
        // The stub reports 800x600 at the origin and 640x480 at (800, 100).
        let backend = StubBackend::with_two_displays();
        let context = filtered("2");

        // On the second monitor: allowed, and the click really happened.
        let response = dispatch_with_context(&backend, Action::Move { x: 900, y: 300 }, &context);
        assert!(response.ok, "{response:?}");

        // On the first monitor, so outside the filter: invalid_arguments, and
        // nothing moved.
        let before = backend.moves.load(Ordering::SeqCst);
        for (x, y) in [(0, 0), (799, 599), (1440, 580), (800, 99), (900, 90)] {
            let response = dispatch_with_context(&backend, Action::Move { x, y }, &context);
            assert_eq!(code_of(&response), "invalid_arguments", "{x},{y}");
            assert_eq!(backend.moves.load(Ordering::SeqCst), before);
        }
        let message = dispatch_with_context(&backend, Action::Move { x: 0, y: 0 }, &context)
            .to_value()["error"]["message"]
            .as_str()
            .expect("message")
            .to_string();
        assert!(message.contains("640x480+800+100"), "{message}");
    }

    #[test]
    fn display_filter_covers_click_move_drag_scroll_and_mouse_halves() {
        let backend = StubBackend::with_two_displays();
        let context = filtered("2");
        let inside = Action::Click {
            x: Some(900),
            y: Some(300),
            button: MouseButton::Left,
            double: false,
            count: 1,
        };
        let outside = Action::Click {
            x: Some(10),
            y: Some(10),
            button: MouseButton::Left,
            double: false,
            count: 1,
        };

        assert!(dispatch_with_context(&backend, inside.clone(), &context).ok);
        assert_eq!(
            code_of(&dispatch_with_context(&backend, outside.clone(), &context)),
            "invalid_arguments"
        );
        assert_eq!(
            code_of(&dispatch_with_context(
                &backend,
                Action::DoubleClick {
                    x: Some(10),
                    y: Some(10),
                    button: MouseButton::Left
                },
                &context
            )),
            "invalid_arguments"
        );
        assert_eq!(
            code_of(&dispatch_with_context(
                &backend,
                Action::MouseDown {
                    x: 10,
                    y: 10,
                    button: MouseButton::Left
                },
                &context
            )),
            "invalid_arguments"
        );
        assert_eq!(
            code_of(&dispatch_with_context(
                &backend,
                Action::Scroll {
                    x: Some(10),
                    y: Some(10),
                    dx: Some(0),
                    dy: Some(1),
                    direction: None,
                    amount: None,
                },
                &context
            )),
            "invalid_arguments"
        );
        let path = vec![Point { x: 900, y: 300 }, Point { x: 10, y: 10 }];
        assert_eq!(
            code_of(&dispatch_with_context(
                &backend,
                Action::Drag {
                    path,
                    button: MouseButton::Left
                },
                &context
            )),
            "invalid_arguments"
        );
    }

    #[test]
    fn a_coordinate_action_without_a_point_passes_the_filter() {
        // Element-targeted and wheel-only actions name no point, so the filter
        // has nothing to reject. (A bare `click` with neither `element` nor
        // `x`/`y` is refused earlier by `preflight_action`, not by this gate.)
        let backend = StubBackend::with_two_displays();
        let response = dispatch_with_context(
            &backend,
            Action::Scroll {
                x: None,
                y: None,
                dx: Some(0),
                dy: Some(2),
                direction: None,
                amount: None,
            },
            &filtered("2"),
        );
        assert!(response.ok, "{response:?}");
        assert_eq!(backend.scrolls.lock().expect("scrolls").len(), 1);
    }

    #[test]
    fn an_unknown_display_id_is_invalid_arguments() {
        let backend = StubBackend::with_two_displays();
        let response =
            dispatch_with_context(&backend, Action::Move { x: 900, y: 300 }, &filtered("9"));
        assert_eq!(code_of(&response), "invalid_arguments");

        // `displays` and `isolation` do not consume the filter, so a valid id
        // that no longer exists still lets them answer.
        let response = dispatch_with_context(&backend, Action::Displays, &filtered("99999"));
        assert!(response.ok, "{response:?}");
        let response = dispatch_with_context(&backend, Action::Isolation, &filtered("99999"));
        assert!(response.ok, "{response:?}");
        assert_eq!(backend.list_displays().unwrap().len(), 2);

        // Bad syntax is refused everywhere, `displays` included.
        let response = dispatch_with_context(&backend, Action::Displays, &filtered("nope"));
        assert_eq!(code_of(&response), "invalid_arguments");
        // `virtual` is valid syntax, and `displays` does not consume the filter,
        // so it answers normally. A command that *would* capture under that
        // filter reports `unsupported` instead.
        let response = dispatch_with_context(&backend, Action::Displays, &filtered("virtual"));
        assert!(response.ok, "{response:?}");
    }

    #[test]
    fn virtual_is_unsupported_over_the_json_path() {
        // The JSON path cannot exec a prepared X server, so it says so instead
        // of silently capturing the whole desktop.
        let backend = StubBackend::default();
        let response = dispatch_with_context(
            &backend,
            Action::Move { x: 10, y: 10 },
            &filtered("virtual"),
        );
        assert_eq!(code_of(&response), "unsupported");

        let response = dispatch_with_context(
            &backend,
            Action::Screenshot {
                output: None,
                region: None,
                format: None,
                max_width: None,
                canvas: None,
                base64: false,
                marks: false,
            },
            &filtered("virtual"),
        );
        assert_eq!(code_of(&response), "unsupported");
    }

    #[test]
    fn the_display_filter_never_leaks_into_displays_or_isolation() {
        let backend = StubBackend::with_two_displays();
        let context = filtered("9");
        assert!(dispatch_with_context(&backend, Action::Displays, &context).ok);
        assert!(dispatch_with_context(&backend, Action::Isolation, &context).ok);
    }

    #[test]
    fn a_screenshot_region_must_fit_inside_the_filtered_monitor() {
        let _cache = crate::cache::CacheEnv::acquire();
        let backend = StubBackend::with_two_displays();
        let context = filtered("2");
        let inside = Action::Screenshot {
            output: None,
            region: Some(Region::new(800, 100, 640, 480).unwrap()),
            format: None,
            max_width: None,
            canvas: None,
            base64: false,
            marks: false,
        };
        let response = dispatch_with_context(&backend, inside, &context);
        assert!(response.ok, "{response:?}");

        // The map origin is the monitor origin, not the desktop origin.
        let map = response.to_value();
        assert_eq!(map["origin"]["x"], 800);
        assert_eq!(map["origin"]["y"], 100);
        let _ = fs::remove_file(
            map["path"]
                .as_str()
                .expect("path")
                .parse::<std::path::PathBuf>()
                .expect("path"),
        );

        let straddling = Action::Screenshot {
            output: None,
            region: Some(Region::new(700, 100, 200, 200).unwrap()),
            format: None,
            max_width: None,
            canvas: None,
            base64: false,
            marks: false,
        };
        assert_eq!(
            code_of(&dispatch_with_context(&backend, straddling, &context)),
            "invalid_arguments"
        );
    }

    #[test]
    fn isolation_reports_the_matrix_and_the_infeasible_keys() {
        let response = dispatch_with(&StubBackend::default(), Action::Isolation);
        assert!(response.ok, "{response:?}");
        let value = response.to_value();
        assert_eq!(value["platform"], crate::isolation::platform());
        assert_eq!(value["display_filter"], true);
        assert_eq!(value["xvfb"], "linux only");
        assert_eq!(value["vm"], "documented, not shipped");
        assert_eq!(
            value["xvfb_recipe"]["server_command"],
            "Xvfb :99 -screen 0 1440x900x24"
        );
        for key in [
            "macos_vm_per_agent",
            "share_host_app_into_guest",
            "hide_virtual_display_from_user",
            "windows_virtual_display",
            "nested_wayland_input_without_portal",
        ] {
            assert!(
                value["infeasible"][key]
                    .as_str()
                    .is_some_and(|s| !s.is_empty()),
                "infeasible.{key} missing"
            );
        }
    }

    #[test]
    fn isolation_is_read_only_and_ignores_the_state_flags() {
        assert_eq!(action_label(&Action::Isolation), "isolation");
        assert!(matches!(
            post_state_kind(&Action::Isolation),
            PostKind::ReadOnly
        ));
        let backend = StubBackend::default();
        let response = dispatch_with_context(
            &backend,
            Action::Isolation,
            &CallContext {
                return_state: true,
                include_screenshot: true,
                ..CallContext::default()
            },
        );
        assert!(response.ok);
        assert!(response.to_value().get("state").is_none());
    }

    #[test]
    fn the_isolation_action_parses_from_json() {
        let request = parse_request(json!({"id": 7, "action": "isolation"})).unwrap();
        assert_eq!(request.id, Some(json!(7)));
        assert!(matches!(request.action, Action::Isolation));
        // The shared `display` key is stripped, never decoded as an action field.
        let request = parse_request(json!({"action": "isolation", "display": "1"})).unwrap();
        assert_eq!(request.context.display.as_deref(), Some("1"));
    }

    fn scroll_action(dx: Option<i32>, dy: Option<i32>) -> Action {
        Action::Scroll {
            x: None,
            y: None,
            dx,
            dy,
            direction: None,
            amount: None,
        }
    }

    /// Compare a JSON float with a tolerance, so `0.05 * 3` is `0.15`.
    fn near(value: f64, expected: f64) -> bool {
        (value - expected).abs() < 1e-9
    }

    fn tier_context(tier: &str) -> CallContext {
        CallContext {
            tier: Some(tier.to_string()),
            ..CallContext::default()
        }
    }

    #[test]
    fn a_scroll_reports_the_tier_that_actually_moved_the_scroll_bar() {
        let backend = StubBackend::default();
        let value = dispatch_with(&backend, scroll_action(None, Some(3))).to_value();
        assert_eq!(value["ok"], json!(true));
        assert_eq!(value["via"], json!("event"));
        assert_eq!(value["scrolled"], json!(true));
        assert_eq!(value["before"], json!(0.0));
        assert!(near(value["after"].as_f64().unwrap_or_default(), 0.15));
        assert_eq!(value["tier"], json!("event"));
        // A delivered wheel event has nothing to warn about.
        assert!(value.get("warnings").is_none());
    }

    #[test]
    fn a_dropped_wheel_event_falls_back_to_the_ax_scroll_bar_and_says_so() {
        let backend = StubBackend {
            scroll_event_delivered: false,
            ..StubBackend::default()
        };
        let value = dispatch_with(&backend, scroll_action(None, Some(2))).to_value();
        assert_eq!(value["ok"], json!(true));
        assert_eq!(value["via"], json!("ax_scrollbar"));
        assert_eq!(value["scrolled"], json!(true));
        assert_eq!(value["before"], json!(0.0));
        assert!(near(value["after"].as_f64().unwrap_or_default(), 0.1));
        assert_eq!(
            value["warnings"][0],
            json!("wheel event not delivered to background app; used AX scroll bar")
        );
    }

    #[test]
    fn a_bar_already_at_its_limit_is_ok_and_never_claims_a_scroll() {
        let backend = StubBackend {
            scroll_event_delivered: false,
            scroll_value: std::sync::Mutex::new(1.0),
            ..StubBackend::default()
        };
        let value = dispatch_with(&backend, scroll_action(None, Some(3))).to_value();
        assert_eq!(value["ok"], json!(true));
        assert_eq!(value["scrolled"], json!(false));
        assert_eq!(value["before"], json!(1.0));
        assert_eq!(value["after"], json!(1.0));
        let warning = value["warnings"][0].as_str().unwrap_or_default();
        assert!(warning.contains("bottom"), "{warning}");
    }

    #[test]
    fn a_scroll_that_cannot_be_verified_is_never_reported_as_a_scroll() {
        let backend = StubBackend {
            scroll_area_missing: true,
            ..StubBackend::default()
        };
        let response = dispatch_with_context(
            &backend,
            scroll_action(None, Some(3)),
            &tier_context("event"),
        );
        assert!(!response.ok);
        assert_eq!(code_of(&response), "execution_failed");
        let message = response.to_value()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(message.contains("cannot be verified"), "{message}");
        assert!(response.to_value().get("scrolled").is_none());
    }

    #[test]
    fn an_ax_refusal_is_a_failure_and_not_a_scroll() {
        let backend = StubBackend {
            scroll_event_delivered: false,
            scroll_ax_refused: true,
            ..StubBackend::default()
        };
        let response = dispatch_with_context(
            &backend,
            scroll_action(None, Some(3)),
            &tier_context("event"),
        );
        assert!(!response.ok);
        assert_eq!(code_of(&response), "execution_failed");
        assert!(
            response.to_value()["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("refused"),
            "{:?}",
            response.to_value()
        );
    }

    #[test]
    fn a_zero_delta_scroll_reports_no_scroll_and_does_not_panic() {
        // `--dx 0 --dy 0` addresses no axis at all. There is no bar to report,
        // so the answer is `scrolled: false` with null values, not a failure.
        for tier in ["event", "ax"] {
            let backend = StubBackend::default();
            let value = dispatch_with_context(
                &backend,
                scroll_action(Some(0), Some(0)),
                &tier_context(tier),
            )
            .to_value();
            assert_eq!(value["ok"], json!(true), "tier {tier}");
            assert_eq!(value["scrolled"], json!(false), "tier {tier}");
            assert_eq!(value["before"], json!(null), "tier {tier}");
            assert_eq!(value["after"], json!(null), "tier {tier}");
            assert!(value.get("warnings").is_none(), "tier {tier}");
        }
    }

    #[test]
    fn an_explicit_ax_scroll_runs_the_scroll_bar_path_without_posting_an_event() {
        let backend = StubBackend::default();
        let response =
            dispatch_with_context(&backend, scroll_action(None, Some(3)), &tier_context("ax"));
        let value = response.to_value();
        assert_eq!(value["ok"], json!(true));
        assert_eq!(value["tier"], json!("ax"));
        assert_eq!(value["via"], json!("ax_scrollbar"));
        assert_eq!(value["scrolled"], json!(true));
        // No wheel event was posted, so the "not delivered" note would be false.
        assert!(
            value.get("warnings").is_none(),
            "unexpected warnings: {:?}",
            value["warnings"]
        );
        assert_eq!(
            *backend.tiers_run.lock().unwrap(),
            vec!["ax".to_string()],
            "tier ax must not fall back to activate"
        );
    }

    #[test]
    fn require_background_never_reaches_the_activate_scroll_tier() {
        // The AX scroll-bar path needs no activation, so it is the only honest
        // answer when the wheel event is not delivered to a background app.
        let backend = StubBackend {
            scroll_event_delivered: false,
            scroll_area_missing: true,
            ..StubBackend::default()
        };
        let context = CallContext {
            tier: Some("event".into()),
            require_background: true,
            ..CallContext::default()
        };
        let response = dispatch_with_context(&backend, scroll_action(None, Some(3)), &context);
        assert!(!response.ok);
        assert_eq!(code_of(&response), "execution_failed");
        let tiers = backend.tiers_run.lock().unwrap().clone();
        assert!(
            !tiers.iter().any(|tier| tier == "activate"),
            "activate must never run: {tiers:?}"
        );
    }

    #[test]
    fn a_horizontal_scroll_reports_the_horizontal_bar() {
        let backend = StubBackend {
            scroll_event_delivered: false,
            scroll_value: std::sync::Mutex::new(0.5),
            ..StubBackend::default()
        };
        let value = dispatch_with(&backend, scroll_action(Some(-2), None)).to_value();
        assert_eq!(value["via"], json!("ax_scrollbar"));
        assert_eq!(value["before"], json!(0.5));
        assert!(near(value["after"].as_f64().unwrap_or_default(), 0.4));

        assert_eq!(value["dx"], json!(-2));
    }

    #[test]
    fn context_helpers_reject_wrong_types() {
        let cases = [
            (json!([]), "request must be a JSON object"),
            (
                json!({"action": "move", "app": true}),
                "app must be a string or number",
            ),
            (
                json!({"action": "move", "diff": "yes"}),
                "diff must be a boolean",
            ),
            (
                json!({"action": "move", "browser_blocked_urls": [1]}),
                "entries must be strings",
            ),
            (
                json!({"action": "move", "browser_blocked_urls": "x"}),
                "must be a list of strings",
            ),
            (
                json!({"action": "move", "element": true}),
                "element must be an integer",
            ),
            (
                json!({"action": "move", "element": "x"}),
                "element must be an integer",
            ),
            (
                json!({"action": "move", "element": -1}),
                "element is out of range",
            ),
            (
                json!({"action": "move", "element": 99999999999u64}),
                "element is out of range",
            ),
        ];
        for (value, needle) in cases {
            let error = parse_request(value).unwrap_err();
            assert!(
                error.to_string().contains(needle),
                "expected {needle:?} in {error}"
            );
        }
    }

    #[test]
    fn drag_path_accepts_a_string_and_rejects_other_shapes() {
        let string = parse_request(json!({"action": "drag", "path": "1,2,3,4"})).unwrap();
        match string.action {
            Action::Drag { path, .. } => assert_eq!(path.len(), 2),
            other => panic!("{other:?}"),
        }
        let array = parse_request(json!({
            "action": "drag",
            "points": [{"x": 1, "y": 2}, {"x": 3, "y": 4}]
        }))
        .unwrap();
        match array.action {
            Action::Drag { path, .. } => assert_eq!(path[1], Point { x: 3, y: 4 }),
            other => panic!("{other:?}"),
        }
        let error = parse_request(json!({"action": "drag", "path": 5})).unwrap_err();
        assert!(error.to_string().contains("path must be"), "{error}");
    }

    #[test]
    fn line_options_apply_every_cli_flag_and_the_request_wins() {
        let response = handle_request_line_opts(
            r#"{"action":"wait","ms":0,"id":7}"#,
            LineOptions {
                adapter: Some("native"),
                policy: Some("/tmp/policy.toml"),
                confirm_cmd: Some("/tmp/confirm"),
                tier: Some("event"),
                allow_foreground: true,
                require_background: true,
                no_overlay: true,
                cdp: true,
                cdp_url: Some("ws://127.0.0.1:1"),
                on_dialog: Some("accept"),
                agent_overlay: Some("on"),
            },
        );
        assert!(response.ok, "{:?}", response.to_value());
        assert_eq!(response.to_value()["id"], 7);

        // A request field wins over the same CLI flag, so a conflicting CLI
        // tier cannot change an already-decided request.
        let response = handle_request_line_opts(
            r#"{"action":"wait","ms":0,"tier":"event","cdp_url":"ws://request","on_dialog":"dismiss","agent_overlay":"off"}"#,
            LineOptions {
                tier: Some("synthetic"),
                cdp_url: Some("ws://cli"),
                on_dialog: Some("accept"),
                agent_overlay: Some("on"),
                ..LineOptions::default()
            },
        );
        assert!(response.ok, "{:?}", response.to_value());
    }

    #[test]
    fn adapter_errors_keep_the_request_id_and_name() {
        let bad_type =
            handle_request_line_with(r#"{"action":"displays","adapter":5,"id":3}"#, None);
        assert_eq!(bad_type.to_value()["error"]["code"], "invalid_arguments");
        assert_eq!(bad_type.to_value()["id"], 3);

        let unknown = handle_request_line_with(r#"{"action":"displays","adapter":"bogus"}"#, None);
        assert_eq!(unknown.to_value()["error"]["code"], "invalid_arguments");
        assert!(unknown.to_value()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("bogus"));

        // A translation failure is stamped with the adapter that produced it.
        let translated = handle_request_line_with(r#"{"action":"wait","adapter":"openai"}"#, None);
        assert_eq!(translated.to_value()["error"]["code"], "invalid_arguments");
        assert_eq!(translated.to_value()["adapter"], "openai");
    }

    #[test]
    fn batch_rejects_non_objects_and_nested_batches() {
        let backend = StubBackend::default();
        let not_object = dispatch_with_context(
            &backend,
            Action::Batch {
                actions: vec![json!(5)],
                stop_on_error: true,
            },
            &CallContext::default(),
        );
        assert_eq!(not_object.to_value()["results"][0]["ok"], false);
        assert!(not_object.to_value()["results"][0]["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("batch step must be a JSON object"));

        let nested = dispatch_with_context(
            &backend,
            Action::Batch {
                actions: vec![json!({"action": "batch", "actions": []})],
                stop_on_error: true,
            },
            &CallContext::default(),
        );
        assert_eq!(nested.to_value()["results"][0]["ok"], false);
        assert!(nested.to_value()["results"][0]["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("nested batch"));
    }

    #[test]
    fn batch_overlay_context_accepts_every_key() {
        let backend = StubBackend::default();
        let response = dispatch_with_context(
            &backend,
            Action::Batch {
                actions: vec![json!({
                    "action": "move",
                    "x": 1,
                    "y": 2,
                    "app": "Notes",
                    "window": "0",
                    "element": 3,
                    "diff": true,
                    "full": true,
                    "coord": "screen",
                    "session": "batch-context",
                    "tier": "event",
                    "adapter": "native",
                    "display": "1",
                    "require_visible": true,
                    "allow_foreground": true,
                    "require_background": true,
                    "no_overlay": true,
                    "return_state": true,
                    "state_diff": true,
                    "include_screenshot": true,
                    "id": 9,
                    "stop_on_error": false
                })],
                stop_on_error: true,
            },
            &CallContext::default(),
        );
        assert!(response.ok, "{:?}", response.to_value());
        assert_eq!(response.to_value()["results"][0]["ok"], true);
    }

    #[test]
    fn click_type_scroll_drag_and_clipboard_validate_input() {
        let backend = StubBackend::default();
        let element_context = CallContext {
            element: Some(3),
            ..CallContext::default()
        };

        let both = dispatch_with_context(
            &backend,
            Action::Click {
                x: Some(1),
                y: Some(2),
                button: MouseButton::Left,
                double: false,
                count: 1,
            },
            &element_context,
        );
        assert_eq!(both.to_value()["error"]["code"], "invalid_arguments");

        let none = dispatch_with_context(
            &backend,
            Action::Click {
                x: None,
                y: None,
                button: MouseButton::Left,
                double: false,
                count: 1,
            },
            &CallContext::default(),
        );
        assert_eq!(none.to_value()["error"]["code"], "invalid_arguments");

        let half = dispatch_with_context(
            &backend,
            Action::Click {
                x: Some(1),
                y: None,
                button: MouseButton::Left,
                double: false,
                count: 1,
            },
            &CallContext::default(),
        );
        assert_eq!(half.to_value()["error"]["code"], "invalid_arguments");

        for (double, count) in [(false, 0), (false, 4), (true, 3)] {
            let response = dispatch_with_context(
                &backend,
                Action::Click {
                    x: Some(1),
                    y: Some(1),
                    button: MouseButton::Left,
                    double,
                    count,
                },
                &CallContext::default(),
            );
            assert_eq!(
                response.to_value()["error"]["code"],
                "invalid_arguments",
                "{double} {count}"
            );
        }
        let doubled = dispatch_with_context(
            &backend,
            Action::Click {
                x: Some(1),
                y: Some(1),
                button: MouseButton::Left,
                double: true,
                count: 1,
            },
            &CallContext::default(),
        );
        assert!(doubled.ok, "{:?}", doubled.to_value());
        assert_eq!(doubled.to_value()["count"], 2);

        let empty = dispatch_with_context(
            &backend,
            Action::Type {
                text: String::new(),
            },
            &CallContext::default(),
        );
        assert_eq!(empty.to_value()["error"]["code"], "invalid_arguments");
        let nul = dispatch_with_context(
            &backend,
            Action::Type {
                text: "a\0b".into(),
            },
            &CallContext::default(),
        );
        assert_eq!(nul.to_value()["error"]["code"], "invalid_arguments");

        let half_point = dispatch_with_context(
            &backend,
            Action::Scroll {
                x: Some(1),
                y: None,
                dx: Some(0),
                dy: Some(1),
                direction: None,
                amount: None,
            },
            &CallContext::default(),
        );
        assert_eq!(half_point.to_value()["error"]["code"], "invalid_arguments");
        let both_forms = dispatch_with_context(
            &backend,
            Action::Scroll {
                x: None,
                y: None,
                dx: Some(1),
                dy: None,
                direction: Some(ScrollDirection::Down),
                amount: None,
            },
            &CallContext::default(),
        );
        assert_eq!(both_forms.to_value()["error"]["code"], "invalid_arguments");
        let amount_only = dispatch_with_context(
            &backend,
            Action::Scroll {
                x: None,
                y: None,
                dx: None,
                dy: None,
                direction: None,
                amount: Some(2),
            },
            &CallContext::default(),
        );
        assert_eq!(amount_only.to_value()["error"]["code"], "invalid_arguments");
        let no_delta = dispatch_with_context(
            &backend,
            Action::Scroll {
                x: None,
                y: None,
                dx: None,
                dy: None,
                direction: None,
                amount: None,
            },
            &CallContext::default(),
        );
        assert_eq!(no_delta.to_value()["error"]["code"], "invalid_arguments");

        let short_drag = dispatch_with_context(
            &backend,
            Action::Drag {
                path: vec![Point { x: 1, y: 2 }],
                button: MouseButton::Left,
            },
            &CallContext::default(),
        );
        assert_eq!(short_drag.to_value()["error"]["code"], "invalid_arguments");

        let long_hold = dispatch_with_context(
            &backend,
            Action::HoldKey {
                keys: "shift".into(),
                ms: crate::tier::MAX_HOLD_MS + 1,
            },
            &CallContext::default(),
        );
        assert_eq!(long_hold.to_value()["error"]["code"], "invalid_arguments");
        let many_hold = dispatch_with_context(
            &backend,
            Action::HoldKey {
                keys: "shift alt".into(),
                ms: 10,
            },
            &CallContext::default(),
        );
        assert_eq!(many_hold.to_value()["error"]["code"], "invalid_arguments");

        let neither = dispatch_with_context(
            &backend,
            Action::ClipboardSet {
                text: None,
                image: None,
            },
            &CallContext::default(),
        );
        assert_eq!(neither.to_value()["error"]["code"], "invalid_arguments");
        let both_clipboard = dispatch_with_context(
            &backend,
            Action::ClipboardSet {
                text: Some("x".into()),
                image: Some("/tmp/x.png".into()),
            },
            &CallContext::default(),
        );
        assert_eq!(
            both_clipboard.to_value()["error"]["code"],
            "invalid_arguments"
        );
    }

    #[test]
    fn wait_element_states_and_scoped_text() {
        let visible = StubBackend::default();
        visible
            .element_views
            .lock()
            .unwrap()
            .push_back(Some(view("AXButton")));
        let response = dispatch_with_context(
            &visible,
            wait_action_with(Some(1), Some("visible"), None, None, None),
            &ctx("Notes"),
        );
        assert!(response.ok, "{:?}", response.to_value());
        assert_eq!(response.to_value()["condition"], "element");

        let enabled = StubBackend::default();
        enabled
            .element_views
            .lock()
            .unwrap()
            .push_back(Some(view("AXButton")));
        let response = dispatch_with_context(
            &enabled,
            wait_action_with(Some(1), Some("enabled"), None, None, None),
            &ctx("Notes"),
        );
        assert!(response.ok, "{:?}", response.to_value());

        let exists = StubBackend::default();
        exists
            .element_views
            .lock()
            .unwrap()
            .push_back(Some(view("AXButton")));
        let response = dispatch_with_context(
            &exists,
            wait_action_with(Some(1), None, None, None, None),
            &ctx("Notes"),
        );
        assert!(response.ok, "{:?}", response.to_value());

        let gone = StubBackend::default();
        gone.element_views.lock().unwrap().push_back(None);
        let response = dispatch_with_context(
            &gone,
            wait_action_with(Some(1), Some("gone"), None, None, None),
            &ctx("Notes"),
        );
        assert!(response.ok, "{:?}", response.to_value());

        let scoped = StubBackend::default();
        scoped
            .element_views
            .lock()
            .unwrap()
            .push_back(Some(view("AXTextField")));
        let response = dispatch_with_context(
            &scoped,
            wait_action_with(None, None, Some("ready"), None, Some(2)),
            &ctx("Notes"),
        );
        assert!(response.ok, "{:?}", response.to_value());
        assert_eq!(response.to_value()["condition"], "text");
    }

    #[test]
    fn window_wait_reports_the_row_and_the_selector() {
        let backend = StubBackend::default();
        let wait_window = |state: &str| Action::Wait {
            ms: None,
            seconds: None,
            element: None,
            element_state: None,
            text: None,
            value: None,
            within_element: None,
            window_state: Some(state.to_string()),
            tree_change: false,
            app_running: None,
            title: None,
            ignore_case: false,
            timeout_ms: Some(150),
            poll_ms: Some(25),
        };
        let exists = dispatch_with_context(&backend, wait_window("exists"), &ctx("Notes"));
        assert!(exists.ok, "{:?}", exists.to_value());
        assert_eq!(exists.to_value()["observed"]["id"], 100);

        // Widget is an accessory app with no windows, so `gone` matches.
        let gone = dispatch_with_context(&backend, wait_window("gone"), &ctx("Widget"));
        assert!(gone.ok, "{:?}", gone.to_value());
        assert!(gone.to_value()["observed"].is_null());

        let mut context = ctx("Notes");
        context.window = Some("1".into());
        let info = dispatch_with_context(&backend, Action::WindowInfo, &context);
        assert!(info.ok, "{:?}", info.to_value());
        assert_eq!(info.to_value()["window"]["title"], "Welcome");

        context.window = Some("99".into());
        let missing = dispatch_with_context(&backend, Action::WindowInfo, &context);
        assert_eq!(missing.to_value()["error"]["code"], "window_not_found");
        assert_eq!(
            missing.to_value()["error"]["message"],
            "window not found: 99"
        );
    }

    #[test]
    fn screenshot_creates_parent_dirs_and_targets_a_window() {
        let cache = crate::cache::CacheEnv::acquire();
        let backend = StubBackend::default();
        let path = cache.dir().join("nested").join("deep").join("shot.png");
        let response = dispatch_with_context(
            &backend,
            Action::Screenshot {
                output: Some(path.display().to_string()),
                region: None,
                format: None,
                max_width: None,
                canvas: None,
                base64: false,
                marks: false,
            },
            &ctx("Notes"),
        );
        assert!(response.ok, "{:?}", response.to_value());
        assert!(path.is_file());
        let value = response.to_value();
        assert_eq!(value["width"], 800);
        assert_eq!(value["height"], 600);

        let both = dispatch_with_context(
            &backend,
            Action::Screenshot {
                output: Some(path.display().to_string()),
                region: Some(Region::new(1, 1, 2, 2).unwrap()),
                format: None,
                max_width: None,
                canvas: None,
                base64: false,
                marks: false,
            },
            &ctx("Notes"),
        );
        assert_eq!(both.to_value()["error"]["code"], "invalid_arguments");

        let zoom = dispatch_with_context(
            &backend,
            Action::Zoom {
                output: Some(path.display().to_string()),
                region: Region::new(10, 10, 20, 20).unwrap(),
                format: None,
                max_width: None,
                canvas: None,
                base64: false,
            },
            &ctx("Notes"),
        );
        assert!(zoom.ok, "{:?}", zoom.to_value());
        assert_eq!(zoom.to_value()["width"], 40);
        assert_eq!(zoom.to_value()["height"], 40);
        let _ = std::fs::remove_dir_all(cache.dir());
    }

    #[test]
    fn menu_dock_apple_event_shortcuts_and_spaces_errors() {
        let backend = StubBackend::default();
        let missing_app = [
            Action::MenuList {
                app: None,
                depth: None,
            },
            Action::MenuClick {
                app: None,
                path: "View > Sidebar".into(),
            },
        ];
        for action in missing_app {
            let response = dispatch_with_context(&backend, action, &CallContext::default());
            assert_eq!(response.to_value()["error"]["code"], "invalid_arguments");
        }

        let empty_path = dispatch_with_context(
            &backend,
            Action::MenuClick {
                app: Some("Notes".into()),
                path: "   ".into(),
            },
            &CallContext::default(),
        );
        assert_eq!(empty_path.to_value()["error"]["code"], "invalid_arguments");

        // A script action is confirm-gated before the app check; allow it so
        // the missing/empty argument is what surfaces.
        let (allow, dir) = allow_confirm_hook();
        let script_context = CallContext {
            confirm_cmd: Some(allow.clone()),
            ..CallContext::default()
        };
        let missing_script_app = dispatch_with_context(
            &backend,
            Action::AppleEvent {
                app: None,
                script: "return 1".into(),
                language: None,
            },
            &script_context,
        );
        assert_eq!(
            missing_script_app.to_value()["error"]["code"],
            "invalid_arguments"
        );
        let empty_script = dispatch_with_context(
            &backend,
            Action::AppleEvent {
                app: Some("Notes".into()),
                script: String::new(),
                language: None,
            },
            &script_context,
        );
        assert_eq!(
            empty_script.to_value()["error"]["code"],
            "invalid_arguments"
        );

        let empty_title = dispatch_with_context(
            &backend,
            Action::DockClick { title: "  ".into() },
            &CallContext::default(),
        );
        assert_eq!(empty_title.to_value()["error"]["code"], "invalid_arguments");

        let empty_name = dispatch_with_context(
            &backend,
            Action::ShortcutsRun { name: "  ".into() },
            &script_context,
        );
        assert_eq!(empty_name.to_value()["error"]["code"], "invalid_arguments");

        let spaces = dispatch_with_context(
            &backend,
            Action::SpacesMove { to_space: 2 },
            &CallContext::default(),
        );
        assert_eq!(spaces.to_value()["error"]["code"], "unsupported");

        let _ = std::fs::remove_dir_all(&dir);
        let cursor =
            dispatch_with_context(&backend, Action::CursorPosition, &CallContext::default());
        assert!(cursor.ok, "{:?}", cursor.to_value());
        assert_eq!(cursor.to_value()["action"], "cursor_position");
    }

    #[test]
    fn secure_key_press_and_sensitive_element_need_a_hook() {
        let backend = StubBackend::default();
        let mut root = crate::tree::UiNode::bare("AXWindow");
        root.title = Some("Notes".into());
        let mut field = crate::tree::UiNode::bare("AXTextField");
        field.secure = true;
        let mut quit = crate::tree::UiNode::bare("AXButton");
        quit.title = Some("Delete".into());
        root.children = vec![field, quit];
        backend.seed_element(root);

        let mut context = ctx("Notes");
        context.element = Some(1);
        let key = dispatch_with_context(&backend, Action::Key { keys: "a".into() }, &context);
        assert_eq!(key.to_value()["error"]["code"], "policy_denied", "{key:?}");

        context.element = Some(2);
        let press = dispatch_with_context(
            &backend,
            Action::Press {
                count: 1,
                button: MouseButton::Left,
            },
            &context,
        );
        assert_eq!(
            press.to_value()["error"]["code"],
            "confirm_required",
            "{press:?}"
        );

        // A press with no element still reaches the element guard.
        let missing = dispatch_with_context(
            &backend,
            Action::Press {
                count: 1,
                button: MouseButton::Left,
            },
            &ctx("Notes"),
        );
        assert_eq!(
            missing.to_value()["error"]["code"],
            "invalid_arguments",
            "{missing:?}"
        );

        // A peek miss is not a denial.
        context.element = Some(99);
        let peek_miss = dispatch_with_context(
            &backend,
            Action::Press {
                count: 1,
                button: MouseButton::Left,
            },
            &context,
        );
        assert_ne!(
            peek_miss.to_value()["error"]["code"],
            "confirm_required",
            "{peek_miss:?}"
        );

        context.element = Some(1);
        let bad_value = dispatch_with_context(
            &backend,
            Action::SetValue {
                value: "a\0b".into(),
            },
            &context,
        );
        assert_eq!(
            bad_value.to_value()["error"]["code"],
            "invalid_arguments",
            "{bad_value:?}"
        );
    }

    #[test]
    fn click_reports_a_failed_hit_test_and_confirms_a_sensitive_title() {
        let failed = StubBackend {
            fail_hit: true,
            ..StubBackend::default()
        };
        let response = dispatch_with_context(
            &failed,
            Action::Click {
                x: Some(1),
                y: Some(1),
                button: MouseButton::Left,
                double: false,
                count: 1,
            },
            &CallContext::default(),
        );
        assert!(response.ok, "{:?}", response.to_value());
        let warnings = response.to_value()["warnings"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .unwrap_or_default();
        assert!(warnings.contains("hit test failed"), "{warnings}");

        let sensitive = StubBackend::default();
        *sensitive.hit.lock().unwrap() = Some(crate::element::HitInfo {
            role: "AXButton".into(),
            title: Some("Delete".into()),
        });
        let response = dispatch_with_context(
            &sensitive,
            Action::Click {
                x: Some(1),
                y: Some(1),
                button: MouseButton::Left,
                double: false,
                count: 1,
            },
            &CallContext::default(),
        );
        assert_eq!(response.to_value()["error"]["code"], "confirm_required");
    }

    #[test]
    fn type_and_input_failure_arms() {
        let backend = StubBackend::default();
        let context = CallContext {
            element: Some(3),
            ..CallContext::default()
        };
        let element = dispatch_with_context(&backend, Action::Type { text: "hi".into() }, &context);
        assert!(element.to_value().get("ok").is_some(), "{element:?}");

        let failing = StubBackend::default();
        *failing.fail_tiers.lock().unwrap() = vec![
            "event".into(),
            "activate".into(),
            "ax".into(),
            "synthetic".into(),
        ];
        for action in [
            Action::Move { x: 1, y: 1 },
            Action::Key {
                keys: "cmd+s".into(),
            },
            Action::Scroll {
                x: None,
                y: None,
                dx: Some(0),
                dy: Some(1),
                direction: None,
                amount: None,
            },
            Action::Drag {
                path: vec![Point { x: 1, y: 1 }, Point { x: 2, y: 2 }],
                button: MouseButton::Left,
            },
            Action::HoldKey {
                keys: "shift".into(),
                ms: 10,
            },
            Action::MouseDown {
                x: 1,
                y: 1,
                button: MouseButton::Left,
            },
            Action::MouseUp {
                x: 1,
                y: 1,
                button: MouseButton::Left,
            },
        ] {
            let response = dispatch_with_context(&failing, action, &CallContext::default());
            assert_eq!(
                response.to_value()["error"]["code"],
                "execution_failed",
                "{response:?}"
            );
        }

        // Guard branches that preflight normally hides.
        assert!(click(
            &backend,
            &CallContext::default(),
            Some(1),
            Some(1),
            MouseButton::Left,
            false,
            0
        )
        .to_value()["error"]["code"]
            .as_str()
            .is_some());
        assert!(drag(
            &backend,
            &CallContext::default(),
            vec![Point { x: 1, y: 1 }],
            MouseButton::Left
        )
        .to_value()["error"]["code"]
            .as_str()
            .is_some());
        assert!(one_hold("shift", crate::tier::MAX_HOLD_MS + 1).is_err());
        assert!(one_hold("shift alt", 10).is_err());
    }

    #[test]
    fn clipboard_text_wait_errors_and_window_guards() {
        let backend = StubBackend::default();
        let (allow, dir) = allow_confirm_hook();
        let clipboard_context = CallContext {
            confirm_cmd: Some(allow),
            ..CallContext::default()
        };
        let text = dispatch_with_context(
            &backend,
            Action::ClipboardSet {
                text: Some("hello".into()),
                image: None,
            },
            &clipboard_context,
        );
        assert!(text.ok, "{:?}", text.to_value());
        assert_eq!(text.to_value()["chars"], 5);

        let failing = StubBackend {
            fail_accessibility: true,
            ..StubBackend::default()
        };
        let wait = dispatch_with_context(
            &failing,
            wait_action_with(None, None, Some("x"), None, None),
            &ctx("Notes"),
        );
        assert!(!wait.ok, "{:?}", wait.to_value());

        let unknown_display = dispatch_with_context(
            &backend,
            Action::WindowMove {
                x: None,
                y: None,
                to_display: Some(999),
            },
            &ctx("Notes"),
        );
        assert_eq!(
            unknown_display.to_value()["error"]["code"],
            "invalid_arguments"
        );
        let missing_y = dispatch_with_context(
            &backend,
            Action::WindowMove {
                x: Some(1),
                y: None,
                to_display: None,
            },
            &ctx("Notes"),
        );
        assert_eq!(missing_y.to_value()["error"]["code"], "invalid_arguments");
        let empty_set = dispatch_with_context(
            &backend,
            Action::WindowSet {
                x: None,
                y: None,
                width: None,
                height: None,
            },
            &ctx("Notes"),
        );
        assert_eq!(empty_set.to_value()["error"]["code"], "invalid_arguments");

        let window_only = CallContext {
            window: Some("0".into()),
            ..CallContext::default()
        };
        let info = dispatch_with_context(&backend, Action::WindowInfo, &window_only);
        assert!(info.ok, "{:?}", info.to_value());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wait_title_without_an_app_uses_the_frontmost_window() {
        let backend = StubBackend::default();
        let response = dispatch_with_context(
            &backend,
            wait_with(None, Some("desk")),
            &CallContext::default(),
        );
        assert!(response.ok, "{:?}", response.to_value());
        assert_eq!(response.to_value()["condition"], "title");
    }

    #[test]
    fn ocr_rejects_missing_targets_and_reports_display_blocks() {
        let backend = StubBackend::default();
        let no_target = dispatch_with_context(
            &backend,
            Action::Ocr {
                force: false,
                image: None,
            },
            &CallContext::default(),
        );
        assert_eq!(no_target.to_value()["error"]["code"], "invalid_arguments");

        let blocked = StubBackend {
            block_display: true,
            ..StubBackend::default()
        };
        let blocked_response = dispatch_with_context(
            &blocked,
            Action::Ocr {
                force: false,
                image: None,
            },
            &ctx("Notes"),
        );
        assert_eq!(
            blocked_response.to_value()["error"]["code"],
            "display_unavailable"
        );

        let outside = StubBackend {
            capture_pixels: Some((400, 300)),
            ..StubBackend::default()
        };
        *outside.ocr_blocks.lock().unwrap() = Some(vec![OcrBlock {
            text: "far".into(),
            x: 99_999,
            y: 99_999,
            width: 1,
            height: 1,
            confidence: 0.1,
        }]);
        let response = dispatch_with_context(
            &outside,
            Action::Ocr {
                force: false,
                image: None,
            },
            &ctx("Notes"),
        );
        assert!(response.ok, "{:?}", response.to_value());
        assert!(response.to_value()["blocks"][0]["screen"].is_null());

        let image_path = format!(
            "{}/tests/fixtures/ocr/hello.png",
            env!("CARGO_MANIFEST_DIR")
        );
        let image_response = dispatch_with_context(
            &outside,
            Action::Ocr {
                force: false,
                image: Some(image_path),
            },
            &CallContext::default(),
        );
        assert!(image_response.ok, "{:?}", image_response.to_value());
        assert!(image_response.to_value()["blocks"][0]["screen"].is_null());
    }

    #[test]
    fn selector_and_policy_helpers_cover_path_pid_and_missing_app() {
        assert_eq!(
            subject_from_selector(&crate::target::AppSelector::Path(std::path::PathBuf::from(
                "/x"
            )))
            .path
            .as_deref(),
            Some("/x")
        );
        assert_eq!(
            subject_from_selector(&crate::target::AppSelector::Pid(42)).pid,
            Some(42)
        );
        let backend = StubBackend::default();
        let missing = dispatch_with_context(&backend, Action::PolicyCheck, &CallContext::default());
        assert_eq!(missing.to_value()["error"]["code"], "invalid_arguments");
        let unknown = dispatch_with_context(&backend, Action::PolicyCheck, &ctx("no-such-app-xyz"));
        assert!(unknown.ok, "{:?}", unknown.to_value());
        assert_eq!(unknown.to_value()["decision"], "allow");
    }

    #[test]
    fn default_valued_fields_are_skipped_when_serialized() {
        let click = Action::Click {
            x: Some(1),
            y: Some(2),
            button: MouseButton::Left,
            double: false,
            count: 1,
        };
        let value = serde_json::to_value(click).unwrap();
        assert!(value.get("double").is_none(), "{value}");
        assert!(value.get("count").is_none(), "{value}");

        let screenshot = Action::Screenshot {
            output: None,
            region: None,
            format: None,
            max_width: None,
            canvas: None,
            base64: false,
            marks: false,
        };
        let value = serde_json::to_value(screenshot).unwrap();
        assert!(value.get("base64").is_none(), "{value}");
        assert!(value.get("marks").is_none(), "{value}");

        let scroll = Action::BrowserScroll {
            selector: None,
            ref_id: None,
            target_id: None,
            dx: 0.0,
            dy: 1.5,
        };
        let value = serde_json::to_value(scroll).unwrap();
        assert!(value.get("dx").is_none(), "{value}");
        assert_eq!(value["dy"], 1.5);

        let batch = Action::BrowserBatch {
            actions: Vec::new(),
            stop_on_error: true,
        };
        let value = serde_json::to_value(batch).unwrap();
        assert!(value.get("stop_on_error").is_none(), "{value}");
    }
}

// -----------------------------------------------------------------------------
// Browser command bridge. The typed driver lives in `crate::browser`; this
// module only lifts the protocol action into a `BrowserCommand` and resolves
// the shared options (policy block list, session, pid).

#[allow(clippy::items_after_test_module)]
fn is_browser_action(action: &Action) -> bool {
    matches!(
        action,
        Action::BrowserTabs {}
            | Action::BrowserActivate { .. }
            | Action::BrowserNavigate { .. }
            | Action::BrowserBack { .. }
            | Action::BrowserForward { .. }
            | Action::BrowserReload { .. }
            | Action::BrowserWait { .. }
            | Action::BrowserSnapshot { .. }
            | Action::BrowserClick { .. }
            | Action::BrowserText { .. }
            | Action::BrowserType { .. }
            | Action::BrowserPress { .. }
            | Action::BrowserHover { .. }
            | Action::BrowserScroll { .. }
            | Action::BrowserSelect { .. }
            | Action::BrowserCheck { .. }
            | Action::BrowserUpload { .. }
            | Action::BrowserDialog { .. }
            | Action::BrowserScreenshot { .. }
            | Action::BrowserEval { .. }
            | Action::BrowserTabNew { .. }
            | Action::BrowserTabClose { .. }
            | Action::BrowserLaunch { .. }
            | Action::BrowserQuit { .. }
            | Action::BrowserSessions {}
            | Action::BrowserBatch { .. }
    )
}

fn browser_options(
    context: &CallContext,
    subject: &policy::Subject,
) -> Result<crate::browser::BrowserOptions, ComputerError> {
    let mut blocked = context.browser_blocked_urls.clone();
    if blocked.is_empty() {
        if let Ok(loaded) = policy::load_policy(context.policy.as_deref().map(Path::new)) {
            blocked = loaded.blocked_urls().to_vec();
        }
    }
    Ok(crate::browser::BrowserOptions {
        debug_url: context.cdp_url.clone(),
        app: context.app.clone(),
        app_pid: subject.pid,
        blocked_urls: blocked,
        timeout_ms: None,
        session: context.session.clone(),
        on_dialog: match context.on_dialog.as_deref() {
            Some(value) => crate::browser::DialogMode::parse(value)?,
            None => crate::browser::DialogMode::Dismiss,
        },
        agent_overlay: match context.agent_overlay.as_deref() {
            Some(value) => crate::browser::AgentOverlay::parse(value)?,
            None => crate::browser::AgentOverlay::Auto,
        },
    })
}

fn parse_wait(
    value: Option<&str>,
    default: crate::browser::WaitSpec,
) -> Result<crate::browser::WaitSpec, ComputerError> {
    match value {
        Some(text) => crate::browser::WaitSpec::parse(text),
        None => Ok(default),
    }
}

fn browser_command(
    action: &Action,
    context: &CallContext,
) -> Result<crate::browser::BrowserCommand, ComputerError> {
    use crate::browser::{BrowserCommand, ScreenshotFormat, WaitSpec};
    if !context.cdp {
        return Err(ComputerError::Unsupported(
            "browser commands require --cdp (or `cdp: true` in the JSON request)".into(),
        ));
    }
    let command = match action {
        Action::BrowserTabs {} => BrowserCommand::Tabs,
        Action::BrowserActivate { target_id } => BrowserCommand::Activate {
            target: target_id.clone().ok_or_else(|| {
                ComputerError::InvalidArguments("browser activate requires --target".into())
            })?,
        },
        Action::BrowserNavigate {
            url,
            target_id,
            wait,
            timeout_ms,
            new_tab,
        } => BrowserCommand::Navigate {
            url: url.clone(),
            target: target_id.clone(),
            wait: parse_wait(wait.as_deref(), WaitSpec::Load)?,
            timeout_ms: *timeout_ms,
            new_tab: *new_tab,
        },
        Action::BrowserBack { target_id } => BrowserCommand::Back {
            target: target_id.clone(),
        },
        Action::BrowserForward { target_id } => BrowserCommand::Forward {
            target: target_id.clone(),
        },
        Action::BrowserReload {
            target_id,
            wait,
            timeout_ms,
        } => BrowserCommand::Reload {
            target: target_id.clone(),
            wait: parse_wait(wait.as_deref(), WaitSpec::Load)?,
            timeout_ms: *timeout_ms,
        },
        Action::BrowserWait {
            target_id,
            wait,
            timeout_ms,
        } => BrowserCommand::Wait {
            target: target_id.clone(),
            wait: WaitSpec::parse(wait)?,
            timeout_ms: *timeout_ms,
        },
        Action::BrowserSnapshot {
            target_id,
            diff,
            interactive_only,
            max_nodes,
        } => BrowserCommand::Snapshot {
            target: target_id.clone(),
            diff: *diff,
            interactive_only: *interactive_only,
            max_nodes: *max_nodes,
        },
        Action::BrowserClick {
            selector,
            ref_id,
            target_id,
            button,
            count,
            wait,
            timeout_ms,
            force,
        } => BrowserCommand::Click {
            target: target_id.clone(),
            selector: selector.clone(),
            ref_id: ref_id.clone(),
            button: *button,
            count: (*count).clamp(1, 3),
            wait: parse_wait(wait.as_deref(), WaitSpec::Load)?,
            timeout_ms: *timeout_ms,
            force: *force,
        },
        Action::BrowserText {
            selector,
            ref_id,
            target_id,
        } => BrowserCommand::Text {
            target: target_id.clone(),
            selector: selector.clone(),
            ref_id: ref_id.clone(),
        },
        Action::BrowserType {
            text,
            selector,
            ref_id,
            target_id,
            clear,
            submit,
            wait,
            timeout_ms,
        } => BrowserCommand::Type {
            target: target_id.clone(),
            selector: selector.clone(),
            ref_id: ref_id.clone(),
            text: text.clone(),
            clear: *clear,
            submit: *submit,
            wait: parse_wait(wait.as_deref(), WaitSpec::Load)?,
            timeout_ms: *timeout_ms,
        },
        Action::BrowserPress {
            keys,
            selector,
            ref_id,
            target_id,
        } => BrowserCommand::Press {
            target: target_id.clone(),
            selector: selector.clone(),
            ref_id: ref_id.clone(),
            keys: keys.clone(),
        },
        Action::BrowserHover {
            selector,
            ref_id,
            target_id,
        } => BrowserCommand::Hover {
            target: target_id.clone(),
            selector: selector.clone(),
            ref_id: ref_id.clone(),
        },
        Action::BrowserScroll {
            selector,
            ref_id,
            target_id,
            dx,
            dy,
        } => BrowserCommand::Scroll {
            target: target_id.clone(),
            selector: selector.clone(),
            ref_id: ref_id.clone(),
            dx: *dx,
            dy: *dy,
        },
        Action::BrowserSelect {
            selector,
            ref_id,
            target_id,
            value,
            label,
        } => BrowserCommand::Select {
            target: target_id.clone(),
            selector: selector.clone(),
            ref_id: ref_id.clone(),
            value: value.clone(),
            label: label.clone(),
        },
        Action::BrowserCheck {
            selector,
            ref_id,
            target_id,
            checked,
        } => BrowserCommand::Check {
            target: target_id.clone(),
            selector: selector.clone(),
            ref_id: ref_id.clone(),
            checked: *checked,
        },
        Action::BrowserUpload {
            selector,
            ref_id,
            target_id,
            files,
        } => BrowserCommand::Upload {
            target: target_id.clone(),
            selector: selector.clone(),
            ref_id: ref_id.clone(),
            files: files.clone(),
        },
        Action::BrowserDialog {
            target_id,
            accept,
            prompt_text,
        } => BrowserCommand::Dialog {
            target: target_id.clone(),
            accept: *accept,
            prompt_text: prompt_text.clone(),
        },
        Action::BrowserScreenshot {
            target_id,
            output,
            full_page,
            format,
            quality,
            base64,
        } => BrowserCommand::Screenshot {
            target: target_id.clone(),
            output: output.clone(),
            full_page: *full_page,
            format: match format.as_deref() {
                Some(value) => ScreenshotFormat::parse(value)?,
                None => ScreenshotFormat::Png,
            },
            quality: *quality,
            base64: *base64,
        },
        Action::BrowserEval {
            script,
            target_id,
            timeout_ms,
            await_promise,
        } => BrowserCommand::Eval {
            target: target_id.clone(),
            script: script.clone(),
            timeout_ms: *timeout_ms,
            await_promise: *await_promise,
        },
        Action::BrowserTabNew { url } => BrowserCommand::TabNew { url: url.clone() },
        Action::BrowserTabClose { target_id } => BrowserCommand::TabClose {
            target: target_id.clone(),
        },
        Action::BrowserLaunch {
            browser,
            headless,
            profile,
            port,
            url,
        } => BrowserCommand::Launch {
            browser: browser.clone(),
            headless: *headless,
            profile: profile.clone(),
            port: *port,
            url: url.clone(),
        },
        Action::BrowserQuit { session, purge } => BrowserCommand::Quit {
            session: session.clone(),
            purge: *purge,
        },
        Action::BrowserSessions {} => BrowserCommand::Sessions,
        Action::BrowserBatch {
            actions,
            stop_on_error,
        } => {
            let mut steps = Vec::with_capacity(actions.len());
            for step in actions {
                if matches!(step, Action::BrowserBatch { .. }) {
                    return Err(ComputerError::InvalidArguments(
                        "nested browser batch is not allowed".into(),
                    ));
                }
                steps.push(browser_command(step, context)?);
            }
            BrowserCommand::Batch {
                steps,
                stop_on_error: *stop_on_error,
            }
        }
        _ => {
            return Err(ComputerError::InvalidArguments(format!(
                "{} is not a browser action",
                context.app.clone().unwrap_or_else(|| "unknown".to_string())
            )))
        }
    };
    Ok(command)
}

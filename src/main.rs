//! `computer-use` command line interface.
//!
//! Every action prints one JSON object on stdout. `--help` and `--version`
//! stay human-readable. `--json` and `serve` read newline-delimited requests.

#![deny(unsafe_code)]

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use computer_use::protocol::{handle_request_line_opts, AppleEventLanguage, LineOptions};
use computer_use::{
    parse_path, parse_region, Action, CallContext, ComputerError, ImageFormat, MouseButton,
    Response, ScrollDirection,
};

/// Cross-platform desktop automation for agents.
///
/// Every action prints one JSON object on stdout:
/// `{"ok":true,...}` or `{"ok":false,"error":{"code":"...","message":"..."}}`.
/// Failed actions exit non-zero.
///
/// `computer-use serve` and `computer-use --json` read newline-delimited JSON
/// requests from stdin and write one JSON response per line. `exec` runs one
/// JSON action. Serve mode always prints compact JSON so each response stays
/// on a single line.
#[derive(Debug, Parser)]
#[command(name = "computer-use", version, propagate_version = true)]
struct Cli {
    /// Pretty-print one-shot JSON. Serve mode stays one object per line.
    #[arg(long, global = true)]
    pretty: bool,

    /// Read newline-delimited JSON actions from stdin and write one JSON response per line.
    #[arg(long)]
    json: bool,

    /// Target application: localized name, bundle id, path, or pid.
    #[arg(long, global = true, value_name = "NAME|BUNDLE|PID")]
    app: Option<String>,

    /// Target window: title, z-order index (0 is front), or `id:<number>`.
    #[arg(long, global = true, value_name = "TITLE|INDEX|ID")]
    window: Option<String>,

    /// Element index from the latest `tree` for this app and window.
    #[arg(long, global = true, value_name = "N")]
    element: Option<u32>,

    /// Fail when the element is outside the window and scroll-into-view cannot run.
    #[arg(long, global = true)]
    require_visible: bool,

    /// Print a diff against the cached tree. This is the default once a baseline exists.
    #[arg(long, global = true, conflicts_with = "full")]
    diff: bool,

    /// Print the full tree. The baseline is always the most recent observation.
    #[arg(long, global = true)]
    full: bool,

    /// Session cache namespace. Defaults to this process id.
    #[arg(long, global = true, value_name = "ID")]
    session: Option<String>,

    /// Attach the post-action accessibility tree.
    #[arg(long = "state", global = true)]
    state: bool,

    /// With `--state`, attach only the post-action delta instead of the full tree.
    ///
    /// The full tree is the expensive part of the payload: on a busy window it
    /// runs to tens of kilobytes of numbered text, while the delta is a few
    /// lines. Agents that drive a loop should prefer this and read
    /// `state.diff`.
    #[arg(long = "state-diff", global = true)]
    state_diff: bool,

    /// With `--state`, also attach a screenshot. A window target captures that window.
    #[arg(long = "screenshot", global = true)]
    include_screenshot: bool,

    /// How to read `x` and `y`: `screen` points or `image` pixels of the last screenshot.
    #[arg(long, global = true, value_name = "screen|image")]
    coord: Option<String>,

    /// Schema translator for `exec`, `serve`, and `--json`.
    #[arg(long, global = true, value_name = "native|openai|anthropic")]
    adapter: Option<String>,

    /// User policy file. Absent uses `~/.config/computer-use/policy.toml`.
    #[arg(long, global = true, value_name = "PATH")]
    policy: Option<PathBuf>,

    /// Program that confirms a sensitive action. Its stdin receives one JSON object.
    #[arg(long, global = true, value_name = "COMMAND")]
    confirm_cmd: Option<String>,

    /// Input tier. Absent is auto: accessibility for presses, event then activate for coordinates.
    #[arg(long, global = true, value_enum, value_name = "TIER")]
    tier: Option<CliTier>,

    /// Alias of `--tier synthetic`. Requires a binary built with `--features skylight`.
    #[arg(long, global = true, conflicts_with = "tier")]
    synthetic_focus: bool,

    /// Skip `foreground.lock` when the tier is `activate`.
    #[arg(long, global = true)]
    allow_foreground: bool,

    /// Stay in the background. Do not fall back to `activate`, and fail if the cursor moves.
    #[arg(long, global = true)]
    require_background: bool,

    /// Do not send the agent point to the overlay helper.
    #[arg(long, global = true)]
    no_overlay: bool,

    /// Route `browser` commands to Chrome DevTools Protocol. Requires a
    /// binary built with `--features cdp` (the release binaries include it).
    /// Attach mode connects to a Chrome the user already started with
    /// `--remote-debugging-port=N`; `browser launch` starts a managed
    /// Chromium with an isolated profile and `browser quit` stops it.
    #[arg(long, global = true)]
    cdp: bool,

    /// Override the discovered CDP websocket URL. Default: scan the `--app`
    /// pid's argv on macOS/Linux and connect to `ws://127.0.0.1:<port>`, or
    /// use the newest `browser launch` session. `http(s)://...` is also
    /// accepted and resolved via `/json/version`.
    #[arg(long, global = true, value_name = "URL")]
    cdp_url: Option<String>,

    /// What to do when a browser JavaScript dialog opens: accept, dismiss,
    /// or fail. Default: dismiss.
    #[arg(long, global = true, value_name = "accept|dismiss|fail")]
    on_dialog: Option<String>,

    /// Show a visible agent marker in a browser page: a badge, a cursor ring,
    /// and a tab title prefix. auto is on for `browser launch`, off for attach.
    #[arg(long, global = true, value_name = "auto|on|off")]
    agent_overlay: Option<String>,

    /// Restrict capture and coordinate conversion to one monitor id from
    /// `displays`. Points stay virtual-desktop points, and a point outside the
    /// monitor is `invalid_arguments`.
    ///
    /// `--display virtual` is different: on Linux it prints the Xvfb recipe and
    /// never captures. See `computer-use isolation`.
    #[arg(long, global = true, value_name = "ID|VIRTUAL")]
    display: Option<String>,

    /// With `--display virtual`, replace this process with a command running
    /// inside the prepared environment (`DISPLAY=:99`). Nothing is started
    /// implicitly: Xvfb must already be running.
    ///
    /// Example: computer-use --display virtual --exec -- computer-use screenshot
    #[arg(long, value_name = "CMD", num_args = 1.., allow_hyphen_values = true)]
    exec: Option<Vec<String>>,

    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Debug, Subcommand)]
enum AuditCommand {
    /// Print the audit log path.
    Path,
    /// Print the newest audit entries, including the rotated backup.
    Tail {
        /// How many lines to return. Default 50, capped at 500.
        #[arg(long, value_name = "N", default_value_t = 50)]
        lines: usize,
    },
}

#[derive(Debug, Subcommand)]
enum SpacesCommand {
    /// Move the selected window to another space. Always `unsupported` in
    /// this build; the private SkyLight API is not called.
    Move {
        /// Managed space index, 1-based, as reported by `spaces`.
        #[arg(long, value_name = "N")]
        to_space: u32,
    },
}

#[derive(Debug, Subcommand)]
enum WindowCommand {
    /// Move the window to `--x`/`--y`, or center it on `--to-display`.
    Move {
        /// Left edge in screen points.
        #[arg(long, value_name = "X")]
        x: Option<i32>,
        /// Top edge in screen points.
        #[arg(long, value_name = "Y")]
        y: Option<i32>,
        /// Center the window on this display id.
        #[arg(long, value_name = "ID", conflicts_with_all = ["x", "y"])]
        to_display: Option<u32>,
    },
    /// Resize the window.
    Resize {
        /// New width in screen points.
        #[arg(long, value_name = "W")]
        width: u32,
        /// New height in screen points.
        #[arg(long, value_name = "H")]
        height: u32,
    },
    /// Set any combination of position and size.
    Set {
        /// Left edge in screen points.
        #[arg(long, value_name = "X")]
        x: Option<i32>,
        /// Top edge in screen points.
        #[arg(long, value_name = "Y")]
        y: Option<i32>,
        /// New width in screen points.
        #[arg(long, value_name = "W")]
        width: Option<u32>,
        /// New height in screen points.
        #[arg(long, value_name = "H")]
        height: Option<u32>,
    },
    /// Read or set minimized/full-screen state.
    State {
        /// Set `AXMinimized` true.
        #[arg(long, conflicts_with = "restore")]
        minimized: bool,
        /// Set `AXMinimized` false.
        #[arg(long)]
        restore: bool,
        /// Set `AXFullScreen` true.
        #[arg(long, conflicts_with = "windowed")]
        fullscreen: bool,
        /// Set `AXFullScreen` false.
        #[arg(long)]
        windowed: bool,
    },
    /// Report bounds, display, and status. Read-only.
    Info,
}

#[derive(Debug, Subcommand)]
enum SessionCommand {
    /// Restore Electron accessibility flags this process changed and stop observers.
    End,
    /// Remove empty session directories and stale ones.
    ///
    /// An empty directory has no snapshot and no screenshot map. It is removed
    /// even when its owner pid is alive. Content is kept while that pid is
    /// alive. Content whose owner is dead or unknown is removed after `--ttl`
    /// seconds (default 24 hours).
    Prune {
        /// Report matches without deleting them.
        #[arg(long)]
        dry_run: bool,
        /// Age in seconds after which a dead owner's session is stale.
        #[arg(long, default_value_t = 86_400)]
        ttl: u64,
    },
}

#[derive(Debug, Subcommand)]
enum ClipboardCommand {
    /// Read text, or write a clipboard image to the session cache.
    Get,
    /// Replace the clipboard. Requires a confirm hook.
    Set {
        /// Text to write. Empty text is refused.
        #[arg(long, conflicts_with = "image")]
        text: Option<String>,
        /// PNG file to write. Used when `--text` is omitted.
        #[arg(long, value_name = "PNG", conflicts_with = "text")]
        image: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum PolicyCommand {
    /// Report allow or deny for `--app`.
    Check,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// Capture the screen, a region, or one window, and write a PNG or JPEG file.
    ///
    /// `--app` and `--window` capture that window. Without them, the capture is
    /// the full desktop or `--region`. The default max width is 1440.
    /// `--max-width 0` keeps the source width. `--canvas 1440x900` letterboxes.
    /// `--marks` writes numbered rectangles for the latest tree (or OCR) so the
    /// agent can reference elements by index.
    ///
    /// Example: computer-use screenshot --app Notes --window 0 --max-width 1440
    Screenshot {
        /// File to write. Defaults to a path in the system temp directory.
        #[arg(long, visible_alias = "path", value_name = "PATH")]
        output: Option<PathBuf>,

        /// Logical region `x,y,width,height` in the same space as click coordinates.
        #[arg(long, value_name = "X,Y,W,H")]
        region: Option<String>,

        /// Image format. Default: png.
        #[arg(long, value_enum)]
        format: Option<CliFormat>,

        /// Scale the image down so its width is at most N pixels. Default: 1440. 0 keeps the source width.
        #[arg(long, value_name = "N")]
        max_width: Option<u32>,

        /// Letterbox into WxH pixels, for example 1440x900. The image is not scaled up.
        #[arg(long, value_name = "WxH")]
        canvas: Option<String>,

        /// Add a standard base64 `data` field to the JSON result.
        #[arg(long)]
        base64: bool,

        /// Stamp numbered element rectangles on the image (tree indices or OCR `oN`).
        #[arg(long)]
        marks: bool,
    },

    /// Move the pointer to a logical point and click, or press an element.
    ///
    /// `--element N` is an alias of `press`. `--x` and `--y` click that point.
    /// Pass one of those forms, not both.
    ///
    /// Example: computer-use click --x 120 --y 240 --button right --double
    Click {
        /// Logical x coordinate. Required with `--y` when `--element` is omitted.
        #[arg(long)]
        x: Option<i32>,
        /// Logical y coordinate. Required with `--x` when `--element` is omitted.
        #[arg(long)]
        y: Option<i32>,
        /// Mouse button. Default: left.
        #[arg(long, value_enum, default_value = "left")]
        button: CliButton,
        /// Click twice. With `--element`, this presses twice.
        #[arg(long)]
        double: bool,
        /// Click count, 1, 2, or 3. `--count 3` is a triple click.
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=3))]
        count: u8,
    },

    /// Click three times. Alias of `click --count 3`.
    #[command(name = "triple-click", visible_alias = "triple_click")]
    TripleClick {
        /// Logical x coordinate.
        #[arg(long)]
        x: i32,
        /// Logical y coordinate.
        #[arg(long)]
        y: i32,
        /// Mouse button. Default: left.
        #[arg(long, value_enum, default_value = "left")]
        button: CliButton,
    },

    /// Press an element from the current tree. `--count` may be 1, 2, or 3.
    ///
    /// `--button right` runs `AXShowMenu` when the element advertises it.
    /// Otherwise, and for middle, back, and forward, the click is aimed at
    /// the element center.
    ///
    /// Example: computer-use press --app Notes --element 12 --count 2
    Press {
        /// How many times to press. Default: 1.
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=3))]
        count: u8,
        /// Mouse button. Default: left.
        #[arg(long, value_enum, default_value = "left")]
        button: CliButton,
    },

    /// Set an element's value. Secure fields are refused.
    ///
    /// Example: computer-use set-value --element 4 --value "hello"
    #[command(name = "set-value", visible_alias = "set_value")]
    SetValue {
        /// New value.
        #[arg(long)]
        value: String,
    },

    /// Select a character range. Secure fields are refused.
    ///
    /// Example: computer-use select-text --element 4 --start 0 --end 5
    #[command(name = "select-text", visible_alias = "select_text")]
    SelectText {
        /// First selected character.
        #[arg(long)]
        start: i64,
        /// Character after the selection.
        #[arg(long)]
        end: i64,
    },

    /// Perform an allowlisted accessibility action.
    ///
    /// Example: computer-use perform-action --element 9 --name AXShowMenu
    #[command(name = "perform-action", visible_alias = "perform_action")]
    PerformAction {
        /// Action name, such as `AXShowMenu` or `AXPress`.
        #[arg(long)]
        name: String,
    },

    /// Scroll an element into view.
    ///
    /// Example: computer-use scroll-to --element 9
    #[command(
        name = "scroll-to",
        visible_alias = "scroll-to-visible",
        aliases = ["scroll_to", "scroll_to_visible"]
    )]
    ScrollTo,

    /// Focus an element.
    ///
    /// Example: computer-use focus --element 4
    Focus,

    /// Move the pointer without clicking.
    ///
    /// Example: computer-use move --x 400 --y 300
    Move {
        /// Logical x coordinate.
        #[arg(long)]
        x: i32,
        /// Logical y coordinate.
        #[arg(long)]
        y: i32,
    },

    /// Drag from the first point through the rest, then release.
    ///
    /// `--path` is a flat `x,y,x,y` list with at least two points.
    ///
    /// Example: computer-use drag --path 10,20,200,40
    Drag {
        /// Flat logical points, `x,y,x,y`.
        #[arg(long, value_name = "X,Y,X,Y")]
        path: String,
        /// Mouse button. Default: left.
        #[arg(long, value_enum, default_value = "left")]
        button: CliButton,
    },

    /// Press a mouse button and leave it down until `mouse-up`.
    ///
    /// A one-shot `mouse-down` releases the button when this process exits.
    /// The button stays down across requests in `serve` and steps in `batch`.
    ///
    /// Example: computer-use mouse-down --x 10 --y 20 --button left
    #[command(name = "mouse-down", visible_alias = "mouse_down")]
    MouseDown {
        /// Logical x coordinate.
        #[arg(long)]
        x: i32,
        /// Logical y coordinate.
        #[arg(long)]
        y: i32,
        /// Mouse button. Default: left.
        #[arg(long, value_enum, default_value = "left")]
        button: CliButton,
    },

    /// Release a mouse button pressed by `mouse-down`.
    ///
    /// Example: computer-use mouse-up --x 10 --y 20
    #[command(name = "mouse-up", visible_alias = "mouse_up")]
    MouseUp {
        /// Logical x coordinate.
        #[arg(long)]
        x: i32,
        /// Logical y coordinate.
        #[arg(long)]
        y: i32,
        /// Mouse button. Default: left.
        #[arg(long, value_enum, default_value = "left")]
        button: CliButton,
    },

    /// Type Unicode text into the focused control.
    ///
    /// Example: computer-use type --text "hello"
    Type {
        /// Text to type.
        #[arg(long)]
        text: String,
    },

    /// Press a key combination.
    ///
    /// Modifiers: cmd, command, meta, super, ctrl, control, alt, option, shift.
    /// Keys: Return, Enter, BackSpace, Delete, Escape, Tab, space, Up, Down,
    /// Left, Right, Home, End, Page_Up, Prior, Page_Down, Next, Insert,
    /// F1–F24, plus, minus, letters, and digits. Names are case-insensitive.
    /// A space separates a sequence, as in `ctrl+a Delete`.
    ///
    /// Example: computer-use key --keys "super+c"
    Key {
        /// Combination such as `super+c` or `ctrl+alt+Delete`.
        #[arg(long, value_name = "COMBO")]
        keys: String,
    },

    /// Hold one key combination, then release it.
    ///
    /// `--ms` above 30000 is `invalid_arguments`. The combo is one xdotool
    /// name, not a space-separated sequence.
    ///
    /// Example: computer-use hold-key --keys shift --ms 400
    #[command(name = "hold-key", visible_alias = "hold_key")]
    HoldKey {
        /// One combination such as `shift` or `ctrl+c`.
        #[arg(long, value_name = "COMBO")]
        keys: String,
        /// Milliseconds to hold. Maximum 30000.
        #[arg(long, value_name = "N")]
        ms: u64,
    },

    /// Read or write the clipboard.
    ///
    /// `get` returns text, an image path, or `kind: empty`. `set` is confirmed.
    /// The audit records the character count and not the text.
    ///
    /// Example: computer-use clipboard set --text "hello"
    Clipboard {
        #[command(subcommand)]
        command: ClipboardCommand,
    },

    /// Screenshot one region. With `--app` or `--window`, the region is
    /// window-local and its far edge may equal the window size.
    ///
    /// Example: computer-use zoom --region 0,0,400,300
    Zoom {
        /// Logical region `x,y,width,height`.
        #[arg(long, value_name = "X,Y,W,H")]
        region: String,
        /// File to write. Defaults to a path in the system temp directory.
        #[arg(long, visible_alias = "path", value_name = "PATH")]
        output: Option<PathBuf>,
        /// Image format. Default: png.
        #[arg(long, value_enum)]
        format: Option<CliFormat>,
        /// Scale the image down so its width is at most N pixels. Default: 1440. 0 keeps the source width.
        #[arg(long, value_name = "N")]
        max_width: Option<u32>,
        /// Letterbox into WxH pixels, for example 1440x900. The image is not scaled up.
        #[arg(long, value_name = "WxH")]
        canvas: Option<String>,
        /// Add a standard base64 `data` field to the JSON result.
        #[arg(long)]
        base64: bool,
    },

    /// Scroll the mouse wheel.
    ///
    /// Use `--dx` and `--dy`, or `--direction` with optional `--amount`.
    /// Positive dy scrolls down. Positive dx scrolls right.
    /// Optional `--x` and `--y` move the pointer first and must be passed together.
    /// Negative numbers are accepted both as `--dy=-3` and as the separate
    /// argument `--dy -3`.
    ///
    /// On macOS the result is verified, not assumed: a wheel event posted to a
    /// non-frontmost app is never delivered to its views. The response reports
    /// `via` (`event` or `ax_scrollbar`), `scrolled`, and the scroll bar's
    /// `before` / `after` values. When the wheel event is not delivered, the
    /// accessibility scroll bar is driven instead and a warning says so.
    ///
    /// Example: computer-use scroll --direction down --amount 3
    /// Example: computer-use scroll --app Notes --dy 3 --tier event
    Scroll {
        /// Move the pointer to this logical x before scrolling.
        #[arg(long, allow_negative_numbers = true)]
        x: Option<i32>,
        /// Move the pointer to this logical y before scrolling.
        #[arg(long, allow_negative_numbers = true)]
        y: Option<i32>,
        /// Horizontal wheel steps. Positive is right.
        #[arg(long, allow_negative_numbers = true)]
        dx: Option<i32>,
        /// Vertical wheel steps. Positive is down.
        #[arg(long, allow_negative_numbers = true)]
        dy: Option<i32>,
        /// Scroll direction used with `--amount`.
        #[arg(long, value_enum)]
        direction: Option<CliDirection>,
        /// Wheel steps used with `--direction`. Default: 1.
        #[arg(long, allow_negative_numbers = true)]
        amount: Option<i32>,
    },

    /// Report displays, cursor, frontmost window, and a best-effort accessibility tree.
    ///
    /// The command does not panic when a subsystem is unavailable. Missing data
    /// is null and `note` explains why.
    ///
    /// Example: computer-use get-state
    #[command(name = "get-state", visible_alias = "get_state")]
    GetState,

    /// Sleep, or wait for one condition (element, text, value, window, tree
    /// change, app, title).
    ///
    /// Without a condition, pass `--ms`, `--seconds`, or both; the durations
    /// are added. With a condition, `--timeout-ms` defaults to 10000 and
    /// `--poll-ms` to 100.
    ///
    /// Example: computer-use wait --ms 250
    /// Example: computer-use wait --app Notes --text "Saved" --timeout-ms 5000
    Wait {
        /// Milliseconds to sleep. Without a condition this is the old wait.
        #[arg(long, value_name = "N")]
        ms: Option<u64>,
        /// Seconds to sleep. Fractions are allowed.
        #[arg(long, value_name = "N")]
        seconds: Option<f64>,
        /// Wait until this element index is in `--element-state`.
        #[arg(long, value_name = "N")]
        element: Option<u32>,
        /// Element state: exists (default), gone, visible, enabled.
        #[arg(long = "element-state", value_name = "STATE")]
        element_state: Option<String>,
        /// Wait until the window tree contains this text.
        #[arg(long, value_name = "TEXT")]
        text: Option<String>,
        /// Wait until the element value contains this text. Requires --element.
        #[arg(long, value_name = "TEXT")]
        value: Option<String>,
        /// Restrict --text to this element's subtree.
        #[arg(long, value_name = "N")]
        within_element: Option<u32>,
        /// Wait until the window selector is exists or gone.
        #[arg(long, value_name = "STATE")]
        window_state: Option<String>,
        /// Wait until the tree differs from the session baseline.
        #[arg(long)]
        tree_change: bool,
        /// Wait until an app is running.
        #[arg(long, value_name = "NAME")]
        app_running: Option<String>,
        /// Wait until the window title contains this text.
        #[arg(long, value_name = "TEXT")]
        title: Option<String>,
        /// Case-insensitive text, value, and title comparison.
        #[arg(long)]
        ignore_case: bool,
        /// Total time budget in milliseconds. Default 10000.
        #[arg(long, value_name = "MS")]
        timeout_ms: Option<u64>,
        /// Poll interval in milliseconds. Default 100, minimum 25.
        #[arg(long, value_name = "MS")]
        poll_ms: Option<u64>,
    },

    /// Assert one condition with a short timeout. Failure is `verification_failed`.
    ///
    /// Example: computer-use verify --app Notes --element 12 --element-state enabled
    Verify {
        /// Assert this element index is in `--element-state`.
        #[arg(long, value_name = "N")]
        element: Option<u32>,
        /// Element state: exists (default), gone, visible, enabled.
        #[arg(long = "element-state", value_name = "STATE")]
        element_state: Option<String>,
        /// Assert the window tree contains this text.
        #[arg(long, value_name = "TEXT")]
        text: Option<String>,
        /// Assert the element value contains this text. Requires --element.
        #[arg(long, value_name = "TEXT")]
        value: Option<String>,
        /// Restrict --text to this element's subtree.
        #[arg(long, value_name = "N")]
        within_element: Option<u32>,
        /// Assert the window selector is exists or gone.
        #[arg(long, value_name = "STATE")]
        window_state: Option<String>,
        /// Assert the tree differs from the session baseline.
        #[arg(long)]
        tree_change: bool,
        /// Assert an app is running.
        #[arg(long, value_name = "NAME")]
        app_running: Option<String>,
        /// Assert the window title contains this text.
        #[arg(long, value_name = "TEXT")]
        title: Option<String>,
        /// Case-insensitive comparison.
        #[arg(long)]
        ignore_case: bool,
        /// Timeout in milliseconds. Default 2000.
        #[arg(long, value_name = "MS")]
        timeout_ms: Option<u64>,
        /// Poll interval in milliseconds. Default 100.
        #[arg(long, value_name = "MS")]
        poll_ms: Option<u64>,
    },

    /// List connected displays without capturing pixels.
    ///
    /// Example: computer-use displays
    Displays,

    /// Report what this build can isolate and what it cannot.
    ///
    /// The matrix names the real pieces (`display_filter`, `xvfb`,
    /// `cg_virtual_display`) and lists what 0.2.0 does not ship. It is the
    /// machine-readable half of `docs/ISOLATION.md`.
    ///
    /// Example: computer-use isolation
    Isolation,

    /// List running applications.
    ///
    /// Example: computer-use list-apps --all
    #[command(name = "list-apps", visible_alias = "list_apps")]
    ListApps {
        /// Include accessory and prohibited apps.
        #[arg(long)]
        all: bool,
        /// Filter by name, bundle id, or path. `*` globs are accepted.
        #[arg(long = "match", value_name = "PATTERN")]
        r#match: Option<String>,
    },

    /// List on-screen windows in z-order. Index 0 is front.
    ///
    /// `--app` keeps that application's windows and renumbers z-order.
    /// `ax_matched` is true when an accessibility window shares that row's bounds.
    ///
    /// Example: computer-use list-windows --app Finder
    #[command(name = "list-windows", visible_alias = "list_windows")]
    ListWindows {
        /// Include system windows (menubar, Dock, notifications).
        #[arg(long)]
        all: bool,
    },

    /// Launch an application by name, bundle id, path, or pid.
    ///
    /// The positional name wins over `--app`. The command refuses the built-in
    /// deny list before it asks the system to open anything.
    ///
    /// Example: computer-use launch com.apple.TextEdit
    Launch {
        /// Application name, bundle id, path, or pid.
        #[arg(value_name = "APP")]
        app: Option<String>,
    },

    /// Bring an application forward.
    ///
    /// Requires `--app`. `--window` selects a window and is checked before activation.
    ///
    /// Example: computer-use activate --app Notes --window 0
    Activate,

    /// Ask an application to quit.
    ///
    /// Requires `--app`. loginwindow, WindowServer, and this process are refused.
    /// Allowed apps need a confirm hook. `--force` asks the OS to force-terminate
    /// after that hook exits 0.
    ///
    /// Example: computer-use quit --app Notes
    Quit {
        /// Force-quit after the confirm hook allows it.
        #[arg(long)]
        force: bool,
    },

    /// Show whether policy allows an app. This does not perform an action.
    ///
    /// Example: computer-use policy check --app Terminal
    Policy {
        #[command(subcommand)]
        command: PolicyCommand,
    },

    /// Pause mutating actions until `resume`.
    Pause,

    /// Clear the pause flag.
    Resume,

    /// Report permissions, the TCC identity, and how to fix what is missing.
    ///
    /// The process exits 0 when the report is produced, including when a
    /// permission is missing. `permissions_ok` is false in that case. The
    /// response `ok` field means the report was produced.
    ///
    /// A normal call does not prompt and does not open Settings. `--prompt`
    /// asks macOS to show the permission dialogs. `--request` is an alias.
    /// `--open-settings` opens the first missing pane.
    ///
    /// Stdout is one JSON object. The same summary is in `text` and, for this
    /// one-shot command, on stderr. `exec` and `serve` leave stderr alone.
    ///
    /// Example: computer-use doctor
    Doctor {
        /// Prompt for Accessibility, Screen Recording, Post Event, and Input Monitoring.
        #[arg(long, visible_alias = "request")]
        prompt: bool,
        /// Open System Settings to the first missing permission pane.
        #[arg(long)]
        open_settings: bool,
        /// Linux only: force X11 detection even on a session that exposes
        /// both `DISPLAY` and `WAYLAND_DISPLAY`. Other platforms ignore the
        /// flag.
        #[cfg_attr(not(target_os = "linux"), arg(long, hide = true))]
        #[arg(long)]
        x11: bool,
        /// Also validate notarization of the containing app bundle (macOS).
        #[arg(long)]
        full: bool,
    },

    /// Print the key window's accessibility tree.
    ///
    /// This does not click. `--diff` is the default once a baseline exists.
    /// `--full` prints the whole tree and replaces that baseline.
    ///
    /// Example: computer-use tree --app Notes --window 0
    Tree,

    /// Run OCR on the most recent capture for `--app` or `--window`.
    ///
    /// macOS only. Returns `unsupported` on Linux and Windows (P12). When
    /// Screen Recording is denied the response is `permission_required`.
    /// The recognized text is **never** logged; it lives in the JSON
    /// response body only.
    ///
    /// Example: computer-use ocr --app Notes
    Ocr {
        /// Re-run OCR even when the latest screenshot map is recent.
        #[arg(long)]
        force: bool,
        /// Run OCR on this image file instead of a live capture. `screen`
        /// equals the image pixel. No secure-field redaction applies.
        #[arg(long, value_name = "PATH")]
        image: Option<String>,
    },

    /// Move, resize, and inspect the front (or `--window`-selected) window.
    ///
    /// macOS reads and writes `AXPosition`/`AXSize`. Windows and Linux return
    /// `unsupported` until their native layers land.
    ///
    /// Example: computer-use window info --app Finder
    Window {
        #[command(subcommand)]
        command: WindowCommand,
    },

    /// Walk a running app's menu bar. macOS only.
    ///
    /// Without `--depth` the walk goes four levels deep. The walk uses the
    /// Accessibility API (`AXMenuBar` -> `AXMenuBarItem` -> `AXMenu`).
    /// `--app` selects the running app; bundle id or pid are accepted too.
    /// Title matches are case-insensitive.
    ///
    /// Example: computer-use menu list --app Finder
    #[command(name = "menu")]
    Menu {
        #[command(subcommand)]
        command: MenuCommand,
    },

    /// Send an Apple event script via `osascript`. macOS only.
    ///
    /// The script body is never logged verbatim. `--language javascript`
    /// selects JavaScript for Automation. The script body may contain
    /// `tell application "X" to ...`; the `--app` value is recorded in the
    /// audit line and used by the policy deny list.
    ///
    /// Example: computer-use apple-event --app Finder --script 'get name'
    #[command(name = "apple-event", alias = "apple_event")]
    AppleEvent {
        /// Optional override. Falls back to `--app`.
        #[arg(long, value_name = "APP")]
        app: Option<String>,
        /// Script body passed to `osascript -e`.
        #[arg(long, value_name = "SCRIPT")]
        script: String,
        /// `applescript` (default) or `javascript`.
        #[arg(long, value_enum)]
        language: Option<CliAppleEventLanguage>,
    },

    /// Run a Shortcuts workflow by name. macOS only.
    ///
    /// The `shortcuts` binary is invoked with the workflow name as a single
    /// positional argument. A missing binary is `execution_failed`.
    ///
    /// Example: computer-use shortcuts run "Open URL"
    #[command(name = "shortcuts")]
    Shortcuts {
        #[command(subcommand)]
        command: ShortcutsCommand,
    },

    /// List or click Dock items. macOS only.
    ///
    /// Example: computer-use dock list
    /// Example: computer-use dock click --title Finder
    #[command(name = "dock")]
    Dock {
        #[command(subcommand)]
        command: DockCommand,
    },

    /// Group on-screen windows by the screen that contains each window's
    /// center.
    ///
    /// Mission Control moves are not attempted; the report only describes
    /// the current layout. The skylight feature, when active, may opt into
    /// best-effort space moves; otherwise the report is read-only.
    ///
    /// Example: computer-use spaces
    Spaces {
        #[command(subcommand)]
        command: Option<SpacesCommand>,
    },

    /// Audit log path and tail. Read-only.
    Audit {
        #[command(subcommand)]
        command: AuditCommand,
    },

    /// Session cache and accessibility teardown.
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },

    /// Read newline-delimited JSON actions from stdin and write one JSON response per line.
    ///
    /// Example: printf '%s\n' '{"id":1,"action":"wait","ms":10}' | computer-use serve
    Serve,

    /// Run one JSON action from an argument.
    ///
    /// Example: computer-use exec '{"action":"wait","ms":10}'
    Exec {
        /// JSON object with an `action` field and optional `id`.
        json: String,
    },

    /// Run a batch of JSON actions from an argument.
    ///
    /// The argument is a JSON object whose `action` field is `batch` and whose
    /// `actions` array holds the steps. Every step shares one target and one
    /// cache revision sequence, so element indices are re-validated between
    /// steps. A nested `batch` is rejected.
    ///
    /// Example: computer-use batch '{"action":"batch","actions":[{"action":"wait","ms":10}]}'
    Batch {
        /// JSON object with an `action` field of `batch` and an `actions` array.
        json: String,
    },

    /// Print the pointer position.
    ///
    /// Example: computer-use cursor-position
    #[command(name = "cursor-position", alias = "cursor_position")]
    CursorPosition,

    /// Local agent-cursor helper. This is not a network service.
    Overlay {
        #[command(subcommand)]
        command: OverlayCommand,
    },

    /// Chrome DevTools Protocol (CDP) subcommands. Requires `--cdp`; the
    /// release binaries include the feature.
    ///
    /// Attach mode connects to a Chrome the user started with
    /// `--remote-debugging-port=N` (and, on Chrome 136+, a non-default
    /// `--user-data-dir`). `browser launch` starts a managed Chromium with
    /// an isolated profile and an ephemeral debug port; `browser quit`
    /// stops it. Pass `--cdp-url` to override the endpoint.
    ///
    /// URLs are matched against `[browser].blocked_urls` in the policy
    /// before they leave this process. A match returns `blocked_url` and the
    /// websocket is never opened.
    ///
    /// `browser snapshot` returns a compact tree with `ref=eN` handles;
    /// actions accept `--selector` or `--ref`. `browser batch` runs a whole
    /// flow in one call. `--agent-overlay` shows a badge, a cursor ring, a
    /// robot favicon, and a 🤖 tab title prefix.
    ///
    /// Example: computer-use --cdp browser tabs
    /// Example: computer-use --cdp browser launch --headless
    /// Example: computer-use --cdp --app "Google Chrome" browser navigate --url "https://example.com"
    #[command(name = "browser", subcommand)]
    Browser(#[command(subcommand)] CliBrowserCommand),
}

#[derive(Debug, Subcommand)]
enum CliBrowserCommand {
    /// List the targets the browser exposes. Filter by `kind == "page"`.
    Tabs,

    /// Bring a tab to the front.
    Activate {
        /// CDP `targetId` from `browser tabs`.
        #[arg(long, value_name = "TARGET")]
        target: String,
    },

    /// Navigate a tab to `--url`.
    Navigate {
        /// Destination URL.
        #[arg(long, value_name = "URL")]
        url: String,
        /// Restrict to one tab.
        #[arg(long, value_name = "TARGET")]
        target: Option<String>,
        /// `none`, `load`, `domcontentloaded`, `networkidle`,
        /// `selector:<css>`, or `url:<glob>`. Default: load.
        #[arg(long, value_name = "WAIT")]
        wait: Option<String>,
        /// Timeout in milliseconds. Default: 30000.
        #[arg(long, value_name = "MS")]
        timeout_ms: Option<u64>,
        /// Open the URL in a new tab.
        #[arg(long)]
        new_tab: bool,
    },

    /// Go back in the tab's history.
    Back {
        /// Restrict to one tab.
        #[arg(long, value_name = "TARGET")]
        target: Option<String>,
    },

    /// Go forward in the tab's history.
    Forward {
        /// Restrict to one tab.
        #[arg(long, value_name = "TARGET")]
        target: Option<String>,
    },

    /// Reload the tab.
    Reload {
        /// Restrict to one tab.
        #[arg(long, value_name = "TARGET")]
        target: Option<String>,
        /// Wait condition after the reload. Default: load.
        #[arg(long, value_name = "WAIT")]
        wait: Option<String>,
        /// Timeout in milliseconds.
        #[arg(long, value_name = "MS")]
        timeout_ms: Option<u64>,
    },

    /// Wait for a page condition without navigating.
    Wait {
        /// Restrict to one tab.
        #[arg(long, value_name = "TARGET")]
        target: Option<String>,
        /// `none`, `load`, `domcontentloaded`, `networkidle`,
        /// `selector:<css>`, or `url:<glob>`.
        #[arg(long, value_name = "WAIT")]
        wait: String,
        /// Timeout in milliseconds. Default: 30000.
        #[arg(long, value_name = "MS")]
        timeout_ms: Option<u64>,
    },

    /// Capture a compact accessibility snapshot with `ref=eN` handles.
    Snapshot {
        /// Restrict to one tab.
        #[arg(long, value_name = "TARGET")]
        target: Option<String>,
        /// Return only the delta against the cached snapshot.
        #[arg(long)]
        diff: bool,
        /// Only actionable controls.
        #[arg(long)]
        interactive_only: bool,
        /// Maximum rendered nodes. Default: 400.
        #[arg(long, value_name = "N")]
        max_nodes: Option<usize>,
    },

    /// Click an element by CSS selector or snapshot ref.
    Click {
        /// CSS selector.
        #[arg(long, value_name = "SELECTOR", conflicts_with = "ref_id")]
        selector: Option<String>,
        /// Snapshot ref such as `e12`.
        #[arg(long = "ref", value_name = "REF", conflicts_with = "selector")]
        ref_id: Option<String>,
        /// Restrict to one tab.
        #[arg(long, value_name = "TARGET")]
        target: Option<String>,
        /// Mouse button. Default: left.
        #[arg(long, value_enum, default_value_t = CliButton::Left)]
        button: CliButton,
        /// Click count: 1, 2, or 3. Default: 1.
        #[arg(long, value_name = "N", default_value_t = 1)]
        count: u8,
        /// Wait condition after the click. Default: load.
        #[arg(long, value_name = "WAIT")]
        wait: Option<String>,
        /// Timeout in milliseconds.
        #[arg(long, value_name = "MS")]
        timeout_ms: Option<u64>,
        /// Skip visibility and enabled checks.
        #[arg(long)]
        force: bool,
    },

    /// Read the `innerText` of an element.
    Text {
        /// CSS selector.
        #[arg(long, value_name = "SELECTOR", conflicts_with = "ref_id")]
        selector: Option<String>,
        /// Snapshot ref such as `e12`.
        #[arg(long = "ref", value_name = "REF", conflicts_with = "selector")]
        ref_id: Option<String>,
        /// Restrict to one tab.
        #[arg(long, value_name = "TARGET")]
        target: Option<String>,
    },

    /// Type `text` into an element. Secure fields are refused.
    Type {
        /// Text to type.
        #[arg(long, value_name = "TEXT")]
        text: String,
        /// CSS selector.
        #[arg(long, value_name = "SELECTOR", conflicts_with = "ref_id")]
        selector: Option<String>,
        /// Snapshot ref such as `e12`.
        #[arg(long = "ref", value_name = "REF", conflicts_with = "selector")]
        ref_id: Option<String>,
        /// Restrict to one tab.
        #[arg(long, value_name = "TARGET")]
        target: Option<String>,
        /// Clear the field before typing.
        #[arg(long)]
        clear: bool,
        /// Press Enter after typing.
        #[arg(long)]
        submit: bool,
        /// Wait condition after typing. Default: load.
        #[arg(long, value_name = "WAIT")]
        wait: Option<String>,
        /// Timeout in milliseconds.
        #[arg(long, value_name = "MS")]
        timeout_ms: Option<u64>,
    },

    /// Press a key combination such as `Enter`, `cmd+a`, or `ctrl+shift+Tab`.
    Press {
        /// Combination to press.
        #[arg(long, value_name = "KEYS")]
        keys: String,
        /// Optional element to focus first.
        #[arg(long, value_name = "SELECTOR", conflicts_with = "ref_id")]
        selector: Option<String>,
        /// Optional snapshot ref to focus first.
        #[arg(long = "ref", value_name = "REF", conflicts_with = "selector")]
        ref_id: Option<String>,
        /// Restrict to one tab.
        #[arg(long, value_name = "TARGET")]
        target: Option<String>,
    },

    /// Move the mouse over an element.
    Hover {
        /// CSS selector.
        #[arg(long, value_name = "SELECTOR", conflicts_with = "ref_id")]
        selector: Option<String>,
        /// Snapshot ref.
        #[arg(long = "ref", value_name = "REF", conflicts_with = "selector")]
        ref_id: Option<String>,
        /// Restrict to one tab.
        #[arg(long, value_name = "TARGET")]
        target: Option<String>,
    },

    /// Scroll the page or an element.
    Scroll {
        /// CSS selector.
        #[arg(long, value_name = "SELECTOR", conflicts_with = "ref_id")]
        selector: Option<String>,
        /// Snapshot ref.
        #[arg(long = "ref", value_name = "REF", conflicts_with = "selector")]
        ref_id: Option<String>,
        /// Restrict to one tab.
        #[arg(long, value_name = "TARGET")]
        target: Option<String>,
        /// Horizontal wheel delta.
        #[arg(long, default_value_t = 0.0)]
        dx: f64,
        /// Vertical wheel delta. Positive is down.
        #[arg(long, default_value_t = 0.0)]
        dy: f64,
    },

    /// Select an option in a `<select>`.
    Select {
        /// CSS selector.
        #[arg(long, value_name = "SELECTOR", conflicts_with = "ref_id")]
        selector: Option<String>,
        /// Snapshot ref.
        #[arg(long = "ref", value_name = "REF", conflicts_with = "selector")]
        ref_id: Option<String>,
        /// Restrict to one tab.
        #[arg(long, value_name = "TARGET")]
        target: Option<String>,
        /// Option value.
        #[arg(long, value_name = "VALUE")]
        value: Option<String>,
        /// Option label.
        #[arg(long, value_name = "LABEL")]
        label: Option<String>,
    },

    /// Check a checkbox or radio. Pass `--unchecked` to clear it.
    Check {
        /// CSS selector.
        #[arg(long, value_name = "SELECTOR", conflicts_with = "ref_id")]
        selector: Option<String>,
        /// Snapshot ref.
        #[arg(long = "ref", value_name = "REF", conflicts_with = "selector")]
        ref_id: Option<String>,
        /// Restrict to one tab.
        #[arg(long, value_name = "TARGET")]
        target: Option<String>,
        /// Uncheck instead of check.
        #[arg(long)]
        unchecked: bool,
    },

    /// Set files on an `<input type=file>`.
    Upload {
        /// CSS selector.
        #[arg(long, value_name = "SELECTOR", conflicts_with = "ref_id")]
        selector: Option<String>,
        /// Snapshot ref.
        #[arg(long = "ref", value_name = "REF", conflicts_with = "selector")]
        ref_id: Option<String>,
        /// Restrict to one tab.
        #[arg(long, value_name = "TARGET")]
        target: Option<String>,
        /// File path. Repeat for several files.
        #[arg(long = "file", value_name = "PATH", action = clap::ArgAction::Append)]
        files: Vec<String>,
    },

    /// Accept or dismiss a JavaScript dialog.
    Dialog {
        /// Restrict to one tab.
        #[arg(long, value_name = "TARGET")]
        target: Option<String>,
        /// Accept the dialog. Default when neither flag is given.
        #[arg(long, conflicts_with = "dismiss")]
        accept: bool,
        /// Dismiss the dialog.
        #[arg(long)]
        dismiss: bool,
        /// Text for a prompt dialog.
        #[arg(long, value_name = "TEXT")]
        prompt_text: Option<String>,
    },

    /// Capture a page screenshot.
    Screenshot {
        /// Restrict to one tab.
        #[arg(long, value_name = "TARGET")]
        target: Option<String>,
        /// Output path. Defaults to a temp file.
        #[arg(long, value_name = "PATH")]
        output: Option<String>,
        /// Capture beyond the viewport.
        #[arg(long)]
        full_page: bool,
        /// `png` (default) or `jpeg`.
        #[arg(long, value_name = "FORMAT")]
        format: Option<String>,
        /// JPEG quality 0-100.
        #[arg(long, value_name = "N")]
        quality: Option<i64>,
        /// Include the image as base64 in the response. Off by default.
        #[arg(long)]
        base64: bool,
    },

    /// Evaluate JavaScript in the page context.
    Eval {
        /// JavaScript source.
        #[arg(long, value_name = "SCRIPT")]
        eval: String,
        /// Restrict to one tab.
        #[arg(long, value_name = "TARGET")]
        target: Option<String>,
        /// Timeout in milliseconds.
        #[arg(long, value_name = "MS")]
        timeout_ms: Option<u64>,
        /// Await a returned promise.
        #[arg(long)]
        await_promise: bool,
    },

    /// Open a new tab.
    TabNew {
        /// URL to open. Defaults to `about:blank`.
        #[arg(long, value_name = "URL")]
        url: Option<String>,
    },

    /// Close a tab.
    TabClose {
        /// CDP `targetId`.
        #[arg(long, value_name = "TARGET")]
        target: String,
    },

    /// Start a managed Chromium with an isolated profile.
    Launch {
        /// Browser executable name or path.
        #[arg(long, value_name = "NAME|PATH")]
        browser: Option<String>,
        /// Run headless.
        #[arg(long)]
        headless: bool,
        /// Profile directory. Defaults to a cache directory.
        #[arg(long, value_name = "DIR")]
        profile: Option<String>,
        /// Debug port. Default: an ephemeral port.
        #[arg(long, value_name = "PORT")]
        port: Option<u16>,
        /// URL to open. Defaults to `about:blank`.
        #[arg(long, value_name = "URL")]
        url: Option<String>,
    },

    /// Stop a managed Chromium.
    Quit {
        /// Managed session id. Defaults to the newest one.
        #[arg(long, value_name = "ID")]
        session: Option<String>,
        /// Remove the profile directory too.
        #[arg(long)]
        purge: bool,
    },

    /// List managed Chromium sessions.
    Sessions,

    /// Run several browser actions in one call.
    ///
    /// The argument is a JSON array of native browser actions, or an object
    /// with an `actions` array. One process and one permission prompt for the
    /// whole flow.
    Batch {
        /// JSON array of browser actions, or `{"actions":[...]}`.
        #[arg(value_name = "JSON")]
        json: String,
        /// Continue after a failed step instead of stopping.
        #[arg(long)]
        continue_on_error: bool,
    },
}

#[derive(Debug, Subcommand)]
enum OverlayCommand {
    /// Draw a click-through ring at points read from the overlay socket.
    Serve,
}

#[derive(Debug, Subcommand)]
enum MenuCommand {
    /// List the menu bar for one running app.
    ///
    /// `--app` is required. `--depth` caps the recursion; default is 4.
    List {
        /// Optional override. Falls back to `--app`.
        #[arg(long, value_name = "APP")]
        app: Option<String>,
        /// Maximum depth below the menu bar. Default: 4.
        #[arg(long, value_name = "N")]
        depth: Option<u32>,
    },
    /// Click one menu item by `">"`-separated titles.
    ///
    /// `--app` is required. Matches are case-insensitive.
    Click {
        /// Optional override. Falls back to `--app`.
        #[arg(long, value_name = "APP")]
        app: Option<String>,
        /// Path of menu titles separated by `>`.
        #[arg(long, value_name = "PATH")]
        path: String,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliAppleEventLanguage {
    /// AppleScript. Default.
    #[value(name = "applescript", alias = "osa")]
    AppleScript,
    /// JavaScript for Automation. `osascript -l JavaScript`.
    #[value(name = "javascript", alias = "js")]
    JavaScript,
}

impl From<CliAppleEventLanguage> for AppleEventLanguage {
    fn from(value: CliAppleEventLanguage) -> Self {
        match value {
            CliAppleEventLanguage::AppleScript => Self::AppleScript,
            CliAppleEventLanguage::JavaScript => Self::JavaScript,
        }
    }
}

#[derive(Debug, Subcommand)]
enum ShortcutsCommand {
    /// Run a Shortcuts workflow by name. The binary is `shortcuts run <name>`.
    Run {
        /// Workflow name. Passed as a single positional argument.
        name: String,
    },
}

#[derive(Debug, Subcommand)]
enum DockCommand {
    /// List the Dock items in Dock order.
    List,
    /// Click a Dock item by title.
    Click {
        /// Dock item title.
        #[arg(long, value_name = "TITLE")]
        title: String,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliTier {
    /// Accessibility action or value. Does not activate the app.
    #[value(name = "ax")]
    Ax,
    /// Post events to the target process. Does not move the system cursor.
    #[value(name = "event")]
    Event,
    /// Private SkyLight front-process call, then post events. Feature `skylight`.
    #[value(name = "synthetic")]
    Synthetic,
    /// Activate the target, run foreground input, then restore the previous app.
    #[value(name = "activate")]
    Activate,
}

impl CliTier {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ax => "ax",
            Self::Event => "event",
            Self::Synthetic => "synthetic",
            Self::Activate => "activate",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliFormat {
    /// Portable Network Graphics.
    #[value(name = "png")]
    Png,
    /// JPEG. `jpg` is accepted as an alias.
    #[value(name = "jpeg", alias = "jpg")]
    Jpeg,
}

impl From<CliFormat> for ImageFormat {
    fn from(value: CliFormat) -> Self {
        match value {
            CliFormat::Png => Self::Png,
            CliFormat::Jpeg => Self::Jpeg,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliButton {
    #[value(name = "left")]
    Left,
    #[value(name = "right")]
    Right,
    #[value(name = "middle")]
    Middle,
    /// Button 4.
    #[value(name = "back")]
    Back,
    /// Button 5.
    #[value(name = "forward")]
    Forward,
}

impl From<CliButton> for MouseButton {
    fn from(value: CliButton) -> Self {
        match value {
            CliButton::Left => Self::Left,
            CliButton::Right => Self::Right,
            CliButton::Middle => Self::Middle,
            CliButton::Back => Self::Back,
            CliButton::Forward => Self::Forward,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliDirection {
    #[value(name = "up")]
    Up,
    #[value(name = "down")]
    Down,
    #[value(name = "left")]
    Left,
    #[value(name = "right")]
    Right,
}

impl From<CliDirection> for ScrollDirection {
    fn from(value: CliDirection) -> Self {
        match value {
            CliDirection::Up => Self::Up,
            CliDirection::Down => Self::Down,
            CliDirection::Left => Self::Left,
            CliDirection::Right => Self::Right,
        }
    }
}

fn main() -> ExitCode {
    let code = run();
    computer_use::shutdown_sessions();
    ExitCode::from(code)
}

fn run() -> u8 {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => match error.kind() {
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => error.exit(),
            _ => {
                emit(
                    &Response::failure("invalid_arguments", clap_message(&error)),
                    false,
                );
                return 2;
            }
        },
    };

    match (&cli.command, cli.json) {
        (Some(CliCommand::Exec { json }), _) => {
            let response = handle_request_line_opts(json, line_options(&cli));
            let code = u8::from(!response.ok);
            emit(&response, cli.pretty);
            code
        }
        (Some(CliCommand::Batch { json }), _) => {
            let response = handle_request_line_opts(json, line_options(&cli));
            let code = u8::from(!response.ok);
            emit(&response, cli.pretty);
            code
        }
        // `--display virtual` prepares an environment instead of dispatching an
        // action, so it is answered before the normal command routing. On a
        // platform without a supported recipe this is `unsupported`.
        (_, _)
            if cli.display.as_deref().is_some_and(|spec| {
                spec.eq_ignore_ascii_case(computer_use::isolation::VIRTUAL_SELECTOR)
            }) =>
        {
            virtual_display(&cli)
        }
        (_, _) if cli.exec.is_some() => {
            emit(
                &Response::failure(
                    "invalid_arguments",
                    "--exec only runs with `--display virtual` on Linux",
                ),
                cli.pretty,
            );
            2
        }
        (Some(CliCommand::Serve), _) | (None, true) => serve(&cli),
        (Some(_), true) => {
            emit(
                &Response::failure(
                    "invalid_arguments",
                    "--json reads actions from stdin. Drop the subcommand, or use serve or exec",
                ),
                cli.pretty,
            );
            2
        }
        (None, false) => {
            let mut command = Cli::command();
            let _ = command.print_help();
            println!();
            2
        }
        (
            Some(CliCommand::Overlay {
                command: OverlayCommand::Serve,
            }),
            false,
        ) => match computer_use::overlay::serve() {
            Ok(()) => 0,
            Err(error) => {
                emit(&Response::from_error(error), cli.pretty);
                1
            }
        },
        (Some(command), false) => {
            if let Some(response) = reject_non_native_adapter(cli.adapter.as_deref()) {
                emit(&response, cli.pretty);
                return 1;
            }
            let context = CallContext {
                app: cli.app.clone(),
                window: cli.window.clone(),
                element: cli.element,
                diff: cli.diff,
                full: cli.full,
                session: cli.session.clone(),
                require_visible: cli.require_visible,
                return_state: cli.state || cli.state_diff,
                state_diff: cli.state_diff,
                include_screenshot: cli.include_screenshot,
                coord: cli.coord.clone(),
                policy: cli.policy.as_ref().map(|path| path.display().to_string()),
                confirm_cmd: cli.confirm_cmd.clone(),
                tier: tier_name(&cli).map(str::to_string),
                allow_foreground: cli.allow_foreground,
                require_background: cli.require_background,
                no_overlay: cli.no_overlay,
                cdp: cli.cdp,
                cdp_url: cli.cdp_url.clone(),
                on_dialog: cli.on_dialog.clone(),
                agent_overlay: cli.agent_overlay.clone(),
                display: cli.display.clone(),
                ..CallContext::default()
            };
            match command_action(command, context) {
                Ok((action, context)) => emit_action(action, &context, cli.pretty),
                Err(response) => {
                    emit(&response, cli.pretty);
                    1
                }
            }
        }
    }
}

/// Answer `--display virtual`.
///
/// Prints the Xvfb recipe and, with `--exec`, replaces this process with the
/// prepared command. Xvfb is never started here: an agent must not leave a
/// stray X server behind or race a second one on `:99`.
fn virtual_display(cli: &Cli) -> u8 {
    // Linux only. Off Linux there is no supported recipe, and printing the
    // Xvfb commands would be a worse answer than saying so.
    if !computer_use::isolation::virtual_supported() {
        emit(
            &Response::from_error(ComputerError::Unsupported(format!(
                "--display virtual needs an X server and only Linux has a supported recipe \
                 (current platform: {}); see docs/ISOLATION.md",
                computer_use::isolation::platform()
            ))),
            cli.pretty,
        );
        return 1;
    }
    match cli.exec.as_deref() {
        Some(raw) => {
            let command = computer_use::isolation::exec_arguments(raw);
            match computer_use::isolation::exec_prepared(&command) {
                Ok(never) => match never {},
                Err(error) => {
                    emit(&Response::from_error(error), cli.pretty);
                    1
                }
            }
        }
        None => {
            let mut body = computer_use::isolation::xvfb_recipe();
            body["command"] = serde_json::json!("computer-use");
            emit(&Response::success(body), cli.pretty);
            0
        }
    }
}

fn command_action(
    command: &CliCommand,
    mut context: CallContext,
) -> Result<(Action, CallContext), Response> {
    let action = match command {
        CliCommand::Screenshot {
            output,
            region,
            format,
            max_width,
            canvas,
            base64,
            marks,
        } => Action::Screenshot {
            output: match output {
                Some(path) => Some(
                    path.to_str()
                        .ok_or_else(|| {
                            Response::failure("invalid_arguments", "output path is not valid UTF-8")
                        })?
                        .to_string(),
                ),
                None => None,
            },
            region: match region {
                Some(text) => Some(parse_region(text).map_err(Response::from_error)?),
                None => None,
            },
            format: format.map(ImageFormat::from),
            max_width: *max_width,
            canvas: canvas.clone(),
            base64: *base64,
            marks: *marks,
        },
        CliCommand::Click {
            x,
            y,
            button,
            double,
            count,
        } => Action::Click {
            x: *x,
            y: *y,
            button: MouseButton::from(*button),
            double: *double,
            count: *count,
        },
        CliCommand::TripleClick { x, y, button } => Action::TripleClick {
            x: Some(*x),
            y: Some(*y),
            button: MouseButton::from(*button),
        },
        CliCommand::Press { count, button } => Action::Press {
            count: *count,
            button: MouseButton::from(*button),
        },
        CliCommand::SetValue { value } => Action::SetValue {
            value: value.clone(),
        },
        CliCommand::SelectText { start, end } => Action::SelectText {
            start: *start,
            end: *end,
        },
        CliCommand::PerformAction { name } => Action::PerformAction { name: name.clone() },
        CliCommand::ScrollTo => Action::ScrollTo,
        CliCommand::Focus => Action::Focus,
        CliCommand::Move { x, y } => Action::Move { x: *x, y: *y },
        CliCommand::Drag { path, button } => Action::Drag {
            path: parse_path(path).map_err(Response::from_error)?,
            button: MouseButton::from(*button),
        },
        CliCommand::MouseDown { x, y, button } => Action::MouseDown {
            x: *x,
            y: *y,
            button: MouseButton::from(*button),
        },
        CliCommand::MouseUp { x, y, button } => Action::MouseUp {
            x: *x,
            y: *y,
            button: MouseButton::from(*button),
        },
        CliCommand::Type { text } => Action::Type { text: text.clone() },
        CliCommand::Key { keys } => Action::Key { keys: keys.clone() },
        CliCommand::HoldKey { keys, ms } => Action::HoldKey {
            keys: keys.clone(),
            ms: *ms,
        },
        CliCommand::Clipboard { command } => match command {
            ClipboardCommand::Get => Action::ClipboardGet,
            ClipboardCommand::Set { text, image } => Action::ClipboardSet {
                text: text.clone(),
                image: image.as_ref().map(|path| path.display().to_string()),
            },
        },
        CliCommand::Zoom {
            region,
            output,
            format,
            max_width,
            canvas,
            base64,
        } => Action::Zoom {
            region: parse_region(region).map_err(Response::from_error)?,
            output: match output {
                Some(path) => Some(
                    path.to_str()
                        .ok_or_else(|| {
                            Response::failure("invalid_arguments", "output path is not valid UTF-8")
                        })?
                        .to_string(),
                ),
                None => None,
            },
            format: format.map(ImageFormat::from),
            max_width: *max_width,
            canvas: canvas.clone(),
            base64: *base64,
        },
        CliCommand::Scroll {
            x,
            y,
            dx,
            dy,
            direction,
            amount,
        } => Action::Scroll {
            x: *x,
            y: *y,
            dx: *dx,
            dy: *dy,
            direction: direction.map(ScrollDirection::from),
            amount: *amount,
        },
        CliCommand::GetState => Action::GetState,
        CliCommand::Wait {
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
        } => Action::Wait {
            ms: *ms,
            seconds: *seconds,
            element: *element,
            element_state: element_state.clone(),
            text: text.clone(),
            value: value.clone(),
            within_element: *within_element,
            window_state: window_state.clone(),
            tree_change: *tree_change,
            app_running: app_running.clone(),
            title: title.clone(),
            ignore_case: *ignore_case,
            timeout_ms: *timeout_ms,
            poll_ms: *poll_ms,
        },
        CliCommand::Verify {
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
        } => Action::Verify {
            element: *element,
            element_state: element_state.clone(),
            text: text.clone(),
            value: value.clone(),
            within_element: *within_element,
            window_state: window_state.clone(),
            tree_change: *tree_change,
            app_running: app_running.clone(),
            title: title.clone(),
            ignore_case: *ignore_case,
            timeout_ms: *timeout_ms,
            poll_ms: *poll_ms,
        },
        CliCommand::Displays => Action::Displays,
        CliCommand::Isolation => Action::Isolation,
        CliCommand::ListApps { all, r#match } => Action::ListApps {
            all: *all,
            r#match: r#match.clone(),
        },
        CliCommand::ListWindows { all } => Action::ListWindows { all: *all },
        CliCommand::Launch { app } => {
            match (app, context.app.as_deref()) {
                (Some(positional), Some(flag)) if positional != flag => {
                    return Err(Response::failure(
                        "invalid_arguments",
                        "launch app and --app disagree",
                    ));
                }
                (Some(positional), _) => context.app = Some(positional.clone()),
                (None, None) => {
                    return Err(Response::failure(
                        "invalid_arguments",
                        "launch requires an app name, bundle id, path, or pid",
                    ));
                }
                (None, Some(_)) => {}
            }
            Action::Launch
        }
        CliCommand::Activate => {
            if context
                .app
                .as_deref()
                .is_none_or(|app| app.trim().is_empty())
            {
                return Err(Response::failure(
                    "invalid_arguments",
                    "activate requires --app",
                ));
            }
            Action::Activate
        }
        CliCommand::Quit { force } => {
            if context
                .app
                .as_deref()
                .is_none_or(|app| app.trim().is_empty())
            {
                return Err(Response::failure(
                    "invalid_arguments",
                    "quit requires --app",
                ));
            }
            Action::Quit { force: *force }
        }
        CliCommand::Policy { command } => match command {
            PolicyCommand::Check => {
                if context
                    .app
                    .as_deref()
                    .is_none_or(|app| app.trim().is_empty())
                {
                    return Err(Response::failure(
                        "invalid_arguments",
                        "policy check requires --app",
                    ));
                }
                Action::PolicyCheck
            }
        },
        CliCommand::Pause => Action::Pause,
        CliCommand::Resume => Action::Resume,
        CliCommand::Doctor {
            prompt,
            open_settings,
            x11,
            full,
        } => Action::Doctor {
            prompt: *prompt,
            open_settings: *open_settings,
            x11: *x11,
            full: *full,
        },
        CliCommand::Tree => Action::Tree,
        CliCommand::CursorPosition => Action::CursorPosition,
        CliCommand::Ocr { force, image } => Action::Ocr {
            force: *force,
            image: image.clone(),
        },
        CliCommand::Window { command } => match command {
            WindowCommand::Move { x, y, to_display } => Action::WindowMove {
                x: *x,
                y: *y,
                to_display: *to_display,
            },
            WindowCommand::Resize { width, height } => Action::WindowResize {
                width: *width,
                height: *height,
            },
            WindowCommand::Set {
                x,
                y,
                width,
                height,
            } => Action::WindowSet {
                x: *x,
                y: *y,
                width: *width,
                height: *height,
            },
            WindowCommand::State {
                minimized,
                restore,
                fullscreen,
                windowed,
            } => Action::WindowState {
                minimized: if *minimized {
                    Some(true)
                } else if *restore {
                    Some(false)
                } else {
                    None
                },
                fullscreen: if *fullscreen {
                    Some(true)
                } else if *windowed {
                    Some(false)
                } else {
                    None
                },
            },
            WindowCommand::Info => Action::WindowInfo,
        },
        CliCommand::Menu { command } => match command {
            MenuCommand::List { app, depth } => {
                if let Some(positional) = app {
                    context.app = Some(positional.clone());
                }
                Action::MenuList {
                    app: None,
                    depth: *depth,
                }
            }
            MenuCommand::Click { app, path } => {
                if let Some(positional) = app {
                    context.app = Some(positional.clone());
                }
                Action::MenuClick {
                    app: None,
                    path: path.to_string(),
                }
            }
        },
        CliCommand::AppleEvent {
            app,
            script,
            language,
        } => {
            if let Some(positional) = app {
                context.app = Some(positional.clone());
            }
            Action::AppleEvent {
                app: None,
                script: script.to_string(),
                language: language.map(AppleEventLanguage::from),
            }
        }
        CliCommand::Shortcuts { command } => match command {
            ShortcutsCommand::Run { name } => Action::ShortcutsRun {
                name: name.to_string(),
            },
        },
        CliCommand::Dock { command } => match command {
            DockCommand::List => Action::DockList {},
            DockCommand::Click { title } => Action::DockClick {
                title: title.to_string(),
            },
        },
        CliCommand::Spaces { command } => match command {
            None => Action::Spaces {},
            Some(SpacesCommand::Move { to_space }) => Action::SpacesMove {
                to_space: *to_space,
            },
        },
        CliCommand::Browser(command) => browser_action(command, &mut context)?,
        CliCommand::Audit { command } => match command {
            AuditCommand::Path => Action::AuditPath {},
            AuditCommand::Tail { lines } => Action::AuditTail {
                lines: Some(*lines),
            },
        },
        CliCommand::Session { command } => match command {
            SessionCommand::End => Action::SessionEnd,
            SessionCommand::Prune { dry_run, ttl } => Action::SessionPrune {
                dry_run: *dry_run,
                ttl_secs: Some(*ttl),
            },
        },
        CliCommand::Serve
        | CliCommand::Exec { .. }
        | CliCommand::Batch { .. }
        | CliCommand::Overlay { .. } => {
            return Err(Response::failure(
                "invalid_arguments",
                "internal error: serve, exec, batch, and overlay are not desktop actions",
            ))
        }
    };
    Ok((action, context))
}

/// Convert the `browser` subcommand into a typed `Action`. The dispatcher's
/// policy logic is responsible for blocking URLs and refusing off-platform
/// bundles; this helper just enforces the `--cdp` gate and lifts flags into
/// the action payload.
fn browser_action(
    command: &CliBrowserCommand,
    context: &mut CallContext,
) -> Result<Action, Response> {
    if !context.cdp {
        // The CLI sets this from `--cdp`. Returning a structured invalid-args
        // error is friendlier than letting the dispatcher complain about a
        // missing feature.
        return Err(Response::failure(
            "invalid_arguments",
            "browser subcommands require --cdp",
        ));
    }
    let action = match command {
        CliBrowserCommand::Tabs => Action::BrowserTabs {},
        CliBrowserCommand::Activate { target } => Action::BrowserActivate {
            target_id: Some(target.clone()),
        },
        CliBrowserCommand::Navigate {
            url,
            target,
            wait,
            timeout_ms,
            new_tab,
        } => Action::BrowserNavigate {
            url: url.clone(),
            target_id: target.clone(),
            wait: wait.clone(),
            timeout_ms: *timeout_ms,
            new_tab: *new_tab,
        },
        CliBrowserCommand::Back { target } => Action::BrowserBack {
            target_id: target.clone(),
        },
        CliBrowserCommand::Forward { target } => Action::BrowserForward {
            target_id: target.clone(),
        },
        CliBrowserCommand::Reload {
            target,
            wait,
            timeout_ms,
        } => Action::BrowserReload {
            target_id: target.clone(),
            wait: wait.clone(),
            timeout_ms: *timeout_ms,
        },
        CliBrowserCommand::Wait {
            target,
            wait,
            timeout_ms,
        } => Action::BrowserWait {
            target_id: target.clone(),
            wait: wait.clone(),
            timeout_ms: *timeout_ms,
        },
        CliBrowserCommand::Snapshot {
            target,
            diff,
            interactive_only,
            max_nodes,
        } => Action::BrowserSnapshot {
            target_id: target.clone(),
            diff: *diff,
            interactive_only: *interactive_only,
            max_nodes: *max_nodes,
        },
        CliBrowserCommand::Click {
            selector,
            ref_id,
            target,
            button,
            count,
            wait,
            timeout_ms,
            force,
        } => Action::BrowserClick {
            selector: selector.clone(),
            ref_id: ref_id.clone(),
            target_id: target.clone(),
            button: MouseButton::from(*button),
            count: *count,
            wait: wait.clone(),
            timeout_ms: *timeout_ms,
            force: *force,
        },
        CliBrowserCommand::Text {
            selector,
            ref_id,
            target,
        } => Action::BrowserText {
            selector: selector.clone(),
            ref_id: ref_id.clone(),
            target_id: target.clone(),
        },
        CliBrowserCommand::Type {
            text,
            selector,
            ref_id,
            target,
            clear,
            submit,
            wait,
            timeout_ms,
        } => Action::BrowserType {
            text: text.clone(),
            selector: selector.clone(),
            ref_id: ref_id.clone(),
            target_id: target.clone(),
            clear: *clear,
            submit: *submit,
            wait: wait.clone(),
            timeout_ms: *timeout_ms,
        },
        CliBrowserCommand::Press {
            keys,
            selector,
            ref_id,
            target,
        } => Action::BrowserPress {
            keys: keys.clone(),
            selector: selector.clone(),
            ref_id: ref_id.clone(),
            target_id: target.clone(),
        },
        CliBrowserCommand::Hover {
            selector,
            ref_id,
            target,
        } => Action::BrowserHover {
            selector: selector.clone(),
            ref_id: ref_id.clone(),
            target_id: target.clone(),
        },
        CliBrowserCommand::Scroll {
            selector,
            ref_id,
            target,
            dx,
            dy,
        } => Action::BrowserScroll {
            selector: selector.clone(),
            ref_id: ref_id.clone(),
            target_id: target.clone(),
            dx: *dx,
            dy: *dy,
        },
        CliBrowserCommand::Select {
            selector,
            ref_id,
            target,
            value,
            label,
        } => Action::BrowserSelect {
            selector: selector.clone(),
            ref_id: ref_id.clone(),
            target_id: target.clone(),
            value: value.clone(),
            label: label.clone(),
        },
        CliBrowserCommand::Check {
            selector,
            ref_id,
            target,
            unchecked,
        } => Action::BrowserCheck {
            selector: selector.clone(),
            ref_id: ref_id.clone(),
            target_id: target.clone(),
            checked: !*unchecked,
        },
        CliBrowserCommand::Upload {
            selector,
            ref_id,
            target,
            files,
        } => Action::BrowserUpload {
            selector: selector.clone(),
            ref_id: ref_id.clone(),
            target_id: target.clone(),
            files: files.clone(),
        },
        CliBrowserCommand::Dialog {
            target,
            accept,
            dismiss,
            prompt_text,
        } => Action::BrowserDialog {
            target_id: target.clone(),
            accept: *accept || !*dismiss,
            prompt_text: prompt_text.clone(),
        },
        CliBrowserCommand::Screenshot {
            target,
            output,
            full_page,
            format,
            quality,
            base64,
        } => Action::BrowserScreenshot {
            target_id: target.clone(),
            output: output.clone(),
            full_page: *full_page,
            format: format.clone(),
            quality: *quality,
            base64: *base64,
        },
        CliBrowserCommand::Eval {
            eval,
            target,
            timeout_ms,
            await_promise,
        } => Action::BrowserEval {
            script: eval.clone(),
            target_id: target.clone(),
            timeout_ms: *timeout_ms,
            await_promise: *await_promise,
        },
        CliBrowserCommand::TabNew { url } => Action::BrowserTabNew { url: url.clone() },
        CliBrowserCommand::TabClose { target } => Action::BrowserTabClose {
            target_id: target.clone(),
        },
        CliBrowserCommand::Launch {
            browser,
            headless,
            profile,
            port,
            url,
        } => Action::BrowserLaunch {
            browser: browser.clone(),
            headless: *headless,
            profile: profile.clone(),
            port: *port,
            url: url.clone(),
        },
        CliBrowserCommand::Quit { session, purge } => Action::BrowserQuit {
            session: session.clone(),
            purge: *purge,
        },
        CliBrowserCommand::Sessions => Action::BrowserSessions {},
        CliBrowserCommand::Batch {
            json,
            continue_on_error,
        } => {
            let value: serde_json::Value = serde_json::from_str(json).map_err(|error| {
                Response::failure(
                    "invalid_arguments",
                    format!("browser batch needs JSON: {error}"),
                )
            })?;
            let actions_value = if value.is_array() {
                value
            } else if let Some(actions) = value.get("actions") {
                actions.clone()
            } else {
                return Err(Response::failure(
                    "invalid_arguments",
                    "browser batch needs a JSON array or an object with an actions array",
                ));
            };
            let actions: Vec<Action> = serde_json::from_value(actions_value).map_err(|error| {
                Response::failure(
                    "invalid_arguments",
                    format!("browser batch actions are invalid: {error}"),
                )
            })?;
            Action::BrowserBatch {
                actions,
                stop_on_error: !*continue_on_error,
            }
        }
    };
    Ok(action)
}

fn emit_action(action: Action, context: &CallContext, pretty: bool) -> u8 {
    let doctor = matches!(action, Action::Doctor { .. });
    let response = computer_use::dispatch_context(action, context);
    if doctor {
        if let Some(text) = response
            .to_value()
            .get("text")
            .and_then(|value| value.as_str())
        {
            let mut stderr = io::stderr().lock();
            let _ = writeln!(stderr, "{text}");
        }
    }
    let code = u8::from(!response.ok);
    emit(&response, pretty);
    code
}

fn reject_non_native_adapter(adapter: Option<&str>) -> Option<Response> {
    match adapter {
        None | Some("native") => None,
        Some(name @ ("openai" | "anthropic")) => Some(Response::failure(
            "invalid_arguments",
            format!("--adapter {name} applies to exec, serve, and --json"),
        )),
        Some(other) => Some(Response::failure(
            "invalid_arguments",
            format!("unknown adapter {other:?}"),
        )),
    }
}

fn tier_name(cli: &Cli) -> Option<&'static str> {
    if cli.synthetic_focus {
        Some("synthetic")
    } else {
        cli.tier.map(CliTier::as_str)
    }
}

fn line_options(cli: &Cli) -> LineOptions<'_> {
    LineOptions {
        adapter: cli.adapter.as_deref(),
        policy: cli.policy.as_ref().and_then(|path| path.to_str()),
        confirm_cmd: cli.confirm_cmd.as_deref(),
        tier: tier_name(cli),
        allow_foreground: cli.allow_foreground,
        require_background: cli.require_background,
        no_overlay: cli.no_overlay,
        cdp: cli.cdp,
        cdp_url: cli.cdp_url.as_deref(),
        on_dialog: cli.on_dialog.as_deref(),
        agent_overlay: cli.agent_overlay.as_deref(),
    }
}

fn serve(cli: &Cli) -> u8 {
    let backend = computer_use::create_desktop_backend();
    let _session = computer_use::InputSession::open(backend.as_ref());
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    loop {
        let line = match lines.next() {
            Some(Ok(line)) => line,
            Some(Err(error)) => {
                let response = Response::from_error(computer_use::ComputerError::Io(error));
                let _ = write!(out, "{}", response.to_json_line(false));
                let _ = out.flush();
                return 1;
            }
            None => return 0,
        };
        if line.trim().is_empty() {
            continue;
        }
        let response = handle_request_line_opts(&line, line_options(cli));
        if write!(out, "{}", response.to_json_line(false)).is_err() || out.flush().is_err() {
            return 1;
        }
    }
}

fn emit(response: &Response, pretty: bool) {
    let mut stdout = io::stdout().lock();
    let _ = write!(stdout, "{}", response.to_json_line(pretty));
    let _ = stdout.flush();
}

fn clap_message(error: &clap::Error) -> String {
    strip_ansi(&error.to_string())
}

fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(ch);
    }
    out.trim().to_string()
}

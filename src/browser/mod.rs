//! CDP browser automation: attach or launch a Chromium, navigate with waits,
//! observe a compact snapshot with stable refs, and act on the page.
//!
//! The module is compiled on every platform. Without the `cdp` feature every
//! command returns `unsupported`; with the feature the real driver in
//! [`cdp`] runs.
//!
//! ## Modes
//!
//! * **Attach** (default): connect to a Chromium the user started with
//!   `--remote-debugging-port=N`. `--cdp-url` wins; otherwise the port is
//!   discovered from the `--app` pid's argv on macOS/Linux.
//! * **Managed** (`browser launch`): start a dedicated Chromium with an
//!   isolated profile and `--remote-debugging-port=0`, record the port in the
//!   session cache, and quit it with `browser quit`.

pub mod actions;
pub mod cdp;
pub mod keys;
pub mod launch;
pub mod snapshot;

use serde::{Deserialize, Serialize};

use crate::error::ComputerError;
use crate::types::MouseButton;

/// Options shared by every browser command.
#[derive(Debug, Clone, Default)]
pub struct BrowserOptions {
    /// `--cdp-url` override. Wins over pid discovery.
    pub debug_url: Option<String>,
    /// `--app` name, bundle id, or pid. Used for port discovery.
    pub app: Option<String>,
    /// Pre-resolved pid for the browser process.
    pub app_pid: Option<i32>,
    /// `[browser].blocked_urls` from the loaded policy.
    pub blocked_urls: Vec<String>,
    /// Per-command timeout in milliseconds. Default 30000.
    pub timeout_ms: Option<u64>,
    /// `--session` cache namespace.
    pub session: Option<String>,
    /// What to do when a JavaScript dialog opens.
    pub on_dialog: DialogMode,
    /// Visible agent marker in the page: badge, cursor ring, tab title prefix.
    pub agent_overlay: AgentOverlay,
}

/// When to show the visible agent marker in the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentOverlay {
    /// On for a managed browser, off for an attach.
    #[default]
    Auto,
    /// Always inject the badge, cursor ring, and tab title prefix.
    On,
    /// Never inject it.
    Off,
}

impl AgentOverlay {
    /// Parse `auto`, `on`, or `off`.
    pub fn parse(value: &str) -> Result<Self, ComputerError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "on" => Ok(Self::On),
            "off" => Ok(Self::Off),
            other => Err(ComputerError::InvalidArguments(format!(
                "agent-overlay must be auto, on, or off (got {other})"
            ))),
        }
    }
}

/// JavaScript dialog policy for actions that might open one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DialogMode {
    /// Accept the dialog.
    Accept,
    /// Dismiss the dialog (default).
    #[default]
    Dismiss,
    /// Do not handle it; the action may time out and `browser dialog` can act.
    Fail,
}

impl DialogMode {
    /// Parse `accept`, `dismiss`, or `fail`.
    pub fn parse(value: &str) -> Result<Self, ComputerError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "accept" => Ok(Self::Accept),
            "dismiss" => Ok(Self::Dismiss),
            "fail" => Ok(Self::Fail),
            other => Err(ComputerError::InvalidArguments(format!(
                "on-dialog must be accept, dismiss, or fail (got {other})"
            ))),
        }
    }
}

impl BrowserOptions {
    /// Effective timeout.
    pub fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.timeout_ms.unwrap_or(30_000).max(1))
    }
}

/// What to wait for after a navigation or an action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitSpec {
    /// Return as soon as the CDP command is accepted.
    None,
    /// Wait for the `load` lifecycle event.
    Load,
    /// Wait for the `DOMContentLoaded` lifecycle event.
    DomContentLoaded,
    /// Wait for `load` plus a short quiet period.
    NetworkIdle,
    /// Wait until the selector matches a visible element.
    Selector(String),
    /// Wait until the current URL matches the glob.
    Url(String),
}

impl WaitSpec {
    /// Parse the CLI spelling: `none`, `load`, `domcontentloaded`,
    /// `networkidle`, `selector:<css>`, `url:<glob>`.
    pub fn parse(value: &str) -> Result<Self, ComputerError> {
        let text = value.trim();
        if text.eq_ignore_ascii_case("none") {
            return Ok(Self::None);
        }
        if text.eq_ignore_ascii_case("load") {
            return Ok(Self::Load);
        }
        if text.eq_ignore_ascii_case("domcontentloaded") || text.eq_ignore_ascii_case("dom") {
            return Ok(Self::DomContentLoaded);
        }
        if text.eq_ignore_ascii_case("networkidle") || text.eq_ignore_ascii_case("network-idle") {
            return Ok(Self::NetworkIdle);
        }
        if let Some(selector) = text.strip_prefix("selector:") {
            if selector.trim().is_empty() {
                return Err(ComputerError::InvalidArguments(
                    "wait selector: needs a CSS selector".into(),
                ));
            }
            return Ok(Self::Selector(selector.trim().to_string()));
        }
        if let Some(glob) = text.strip_prefix("url:") {
            if glob.trim().is_empty() {
                return Err(ComputerError::InvalidArguments(
                    "wait url: needs a glob".into(),
                ));
            }
            return Ok(Self::Url(glob.trim().to_string()));
        }
        Err(ComputerError::InvalidArguments(format!(
            "wait must be none, load, domcontentloaded, networkidle, selector:<css>, or url:<glob> (got {value})"
        )))
    }

    /// True when the caller asked to return immediately.
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }
}

/// A typed browser command. The protocol layer maps `Action::Browser*`
/// variants into this enum and calls [`run`].
#[derive(Debug, Clone)]
pub enum BrowserCommand {
    /// List every target the attached browser exposes.
    Tabs,
    /// Bring one tab to the front.
    Activate { target: String },
    /// Navigate a tab to a URL.
    Navigate {
        url: String,
        target: Option<String>,
        wait: WaitSpec,
        timeout_ms: Option<u64>,
        new_tab: bool,
    },
    /// Go back in the tab's history.
    Back { target: Option<String> },
    /// Go forward in the tab's history.
    Forward { target: Option<String> },
    /// Reload the tab.
    Reload {
        target: Option<String>,
        wait: WaitSpec,
        timeout_ms: Option<u64>,
    },
    /// Wait for a condition without navigating.
    Wait {
        target: Option<String>,
        wait: WaitSpec,
        timeout_ms: Option<u64>,
    },
    /// Capture a compact accessibility snapshot with refs.
    Snapshot {
        target: Option<String>,
        diff: bool,
        interactive_only: bool,
        max_nodes: Option<usize>,
    },
    /// Click an element by CSS selector or snapshot ref.
    Click {
        target: Option<String>,
        selector: Option<String>,
        ref_id: Option<String>,
        button: MouseButton,
        count: u8,
        wait: WaitSpec,
        timeout_ms: Option<u64>,
        force: bool,
    },
    /// Read the innerText of an element.
    Text {
        target: Option<String>,
        selector: Option<String>,
        ref_id: Option<String>,
    },
    /// Type text into an element.
    Type {
        target: Option<String>,
        selector: Option<String>,
        ref_id: Option<String>,
        text: String,
        clear: bool,
        submit: bool,
        wait: WaitSpec,
        timeout_ms: Option<u64>,
    },
    /// Press a key combination on the page or an element.
    Press {
        target: Option<String>,
        selector: Option<String>,
        ref_id: Option<String>,
        keys: String,
    },
    /// Move the mouse over an element.
    Hover {
        target: Option<String>,
        selector: Option<String>,
        ref_id: Option<String>,
    },
    /// Scroll the page or an element.
    Scroll {
        target: Option<String>,
        selector: Option<String>,
        ref_id: Option<String>,
        dx: f64,
        dy: f64,
    },
    /// Select an option in a `<select>`.
    Select {
        target: Option<String>,
        selector: Option<String>,
        ref_id: Option<String>,
        value: Option<String>,
        label: Option<String>,
    },
    /// Check or uncheck a checkbox/radio.
    Check {
        target: Option<String>,
        selector: Option<String>,
        ref_id: Option<String>,
        checked: bool,
    },
    /// Set files on an `<input type=file>`.
    Upload {
        target: Option<String>,
        selector: Option<String>,
        ref_id: Option<String>,
        files: Vec<String>,
    },
    /// Accept or dismiss a JavaScript dialog.
    Dialog {
        target: Option<String>,
        accept: bool,
        prompt_text: Option<String>,
    },
    /// Capture a page screenshot.
    Screenshot {
        target: Option<String>,
        output: Option<String>,
        full_page: bool,
        format: ScreenshotFormat,
        quality: Option<i64>,
        base64: bool,
    },
    /// Evaluate JavaScript in the page context.
    Eval {
        target: Option<String>,
        script: String,
        timeout_ms: Option<u64>,
        await_promise: bool,
    },
    /// Open a new tab.
    TabNew { url: Option<String> },
    /// Close a tab by target id.
    TabClose { target: String },
    /// Start a managed Chromium with an isolated profile.
    Launch {
        browser: Option<String>,
        headless: bool,
        profile: Option<String>,
        port: Option<u16>,
        url: Option<String>,
    },
    /// Stop a managed Chromium started by [`BrowserCommand::Launch`].
    Quit {
        session: Option<String>,
        purge: bool,
    },
    /// List managed Chromium sessions.
    Sessions,
    /// Run several browser commands in one call.
    Batch {
        steps: Vec<BrowserCommand>,
        stop_on_error: bool,
    },
}

/// Screenshot format for `browser screenshot`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenshotFormat {
    /// PNG, lossless.
    #[default]
    Png,
    /// JPEG, quality controlled by `quality`.
    Jpeg,
}

impl ScreenshotFormat {
    /// Parse `png` or `jpeg`.
    pub fn parse(value: &str) -> Result<Self, ComputerError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "png" => Ok(Self::Png),
            "jpeg" | "jpg" => Ok(Self::Jpeg),
            other => Err(ComputerError::InvalidArguments(format!(
                "screenshot format must be png or jpeg (got {other})"
            ))),
        }
    }

    /// File extension without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
        }
    }
}

/// Run one browser command.
#[cfg(feature = "cdp")]
pub fn run(
    command: BrowserCommand,
    options: &BrowserOptions,
) -> Result<serde_json::Value, ComputerError> {
    cdp::dispatch(command, options)
}

/// Without the `cdp` feature every browser command is `unsupported`.
#[cfg(not(feature = "cdp"))]
pub fn run(
    _command: BrowserCommand,
    _options: &BrowserOptions,
) -> Result<serde_json::Value, ComputerError> {
    Err(unsupported())
}

/// Stable error used when the binary is built without `--features cdp`.
pub fn unsupported() -> ComputerError {
    ComputerError::Unsupported(
        "browser commands require the `cdp` feature (build with --features cdp)".into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_spec_parses_every_form() {
        assert_eq!(WaitSpec::parse("none").unwrap(), WaitSpec::None);
        assert_eq!(WaitSpec::parse(" LOAD ").unwrap(), WaitSpec::Load);
        assert_eq!(WaitSpec::parse("dom").unwrap(), WaitSpec::DomContentLoaded);
        assert_eq!(
            WaitSpec::parse("network-idle").unwrap(),
            WaitSpec::NetworkIdle
        );
        assert_eq!(
            WaitSpec::parse("selector:#out").unwrap(),
            WaitSpec::Selector("#out".into())
        );
        assert_eq!(
            WaitSpec::parse("url:https://example.com/*").unwrap(),
            WaitSpec::Url("https://example.com/*".into())
        );
        assert!(WaitSpec::parse("selector:").is_err());
        assert!(WaitSpec::parse("url:  ").is_err());
        assert!(WaitSpec::parse("whenever").is_err());
    }

    #[test]
    fn dialog_mode_and_overlay_parse() {
        assert_eq!(DialogMode::parse("accept").unwrap(), DialogMode::Accept);
        assert_eq!(DialogMode::parse("dismiss").unwrap(), DialogMode::Dismiss);
        assert_eq!(DialogMode::parse("fail").unwrap(), DialogMode::Fail);
        assert!(DialogMode::parse("maybe").is_err());
        assert_eq!(AgentOverlay::parse("auto").unwrap(), AgentOverlay::Auto);
        assert_eq!(AgentOverlay::parse("on").unwrap(), AgentOverlay::On);
        assert_eq!(AgentOverlay::parse("off").unwrap(), AgentOverlay::Off);
        assert!(AgentOverlay::parse("sometimes").is_err());
    }
}

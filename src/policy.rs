//! Application policy: built-in deny list plus user and admin `policy.toml`.
//!
//! Resolution order is built-in deny, then admin policy, then user policy.
//! Deny wins. A user allow cannot override an admin deny or a built-in deny.
//! Admin `allow` entries do not grant access. A missing user file is
//! `default = "allow"` plus this built-in list. `pause_on_user_input` defaults
//! to false when the key is absent, so a missing file does not install an event tap.

use std::path::Path;
use std::sync::{OnceLock, RwLock};

use crate::error::ComputerError;
use crate::target::AppSelector;

const SYSTEM_SETTINGS_BUNDLES: &[&str] = &["com.apple.systempreferences", "com.apple.Settings"];

/// Built-in deny entries the loaded policy exempts.
///
/// The built-in list refuses terminals and password managers, plus apps that
/// run agents of their own. A policy file can exempt a specific entry with
/// `[builtin_deny] exempt = [...]`, but only the user file may do so and the
/// exemption is never implicit. The registry is process-wide because the
/// backends consult the built-in list without carrying the policy.
static BUILTIN_EXEMPT: OnceLock<RwLock<Vec<String>>> = OnceLock::new();

fn exempt_registry() -> &'static RwLock<Vec<String>> {
    BUILTIN_EXEMPT.get_or_init(|| RwLock::new(Vec::new()))
}

/// Replace the built-in exemptions with `items`. Called once per action, right
/// after the policy file is loaded.
pub fn set_builtin_exemptions(items: &[String]) {
    let mut registry = exempt_registry()
        .write()
        .unwrap_or_else(|error| error.into_inner());
    *registry = items.to_vec();
}

/// The built-in exemptions currently in force.
pub fn builtin_exemptions() -> Vec<String> {
    exempt_registry()
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

/// True when `value` names a built-in entry the policy exempts.
fn is_exempt(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    builtin_exemptions()
        .iter()
        .any(|item| item.trim().eq_ignore_ascii_case(value))
}

/// True when the exempt list names this bundle id, executable name, or bundle
/// path. The comparison is deliberately loose so one entry can cover the
/// bundle id, the executable, and the `.app` path of the same app.
fn exempts_identity(bundle_id: Option<&str>, exe_name: Option<&str>, bundle: Option<&str>) -> bool {
    let mut candidates: Vec<&str> = Vec::new();
    if let Some(bundle_id) = bundle_id {
        candidates.push(bundle_id);
    }
    if let Some(exe_name) = exe_name {
        candidates.push(exe_name);
        candidates.push(exe_name.trim_end_matches(".exe"));
    }
    if let Some(bundle) = bundle {
        candidates.push(bundle);
        candidates.push(bundle.trim_end_matches(".app"));
    }
    candidates.iter().any(|candidate| is_exempt(candidate))
}

/// Bundle ids that are always refused. Comparison is case-insensitive.
const DENIED_BUNDLE_IDS: &[&str] = &[
    "com.apple.Terminal",
    "com.googlecode.iterm2",
    "dev.warp.Warp-Stable",
    "org.alacritty",
    "net.kovidgoyal.kitty",
    "com.mitchellh.ghostty",
    "com.github.wez.wezterm",
    "co.zeit.hyper",
    "com.1password.1password",
    "com.agilebits.onepassword7",
    "com.bitwarden.desktop",
    "org.keepassxc.KeePassXC",
    "com.dashlane.Dashlane",
    "in.sinew.Enpass-Desktop",
    "com.lastpass.LastPass",
    "com.lastpass.lastpassmacdesktop",
    "com.apple.Passwords",
    "com.apple.keychainaccess",
    "Microsoft.WindowsTerminal",
    "app.crynta.terax",
];

/// Executable file names, matched case-insensitively after canonicalization.
const DENIED_EXE_NAMES: &[&str] = &[
    "Terminal",
    "iTerm2",
    "Warp",
    "Alacritty",
    "kitty",
    "ghostty",
    "wezterm",
    "wezterm-gui",
    "Hyper",
    "hyper",
    "powershell.exe",
    "cmd.exe",
    "wt.exe",
    "WindowsTerminal.exe",
    "gnome-terminal",
    "konsole",
    "xterm",
    "1Password",
    "1Password.exe",
    "Bitwarden",
    "Bitwarden.exe",
    "KeePassXC",
    "KeePassXC.exe",
    "KeePass.exe",
    "Dashlane",
    "Dashlane.exe",
    "Enpass",
    "Enpass.exe",
    "LastPass",
    "lastpass",
    "LastPass.exe",
    "Passwords",
    "Passwords.exe",
    "Keychain Access",
    "Terax",
];

/// `.app` bundle directory names, matched as a single path component.
const DENIED_APP_BUNDLES: &[&str] = &[
    "Terminal.app",
    "iTerm.app",
    "iTerm2.app",
    "Warp.app",
    "Alacritty.app",
    "kitty.app",
    "Ghostty.app",
    "WezTerm.app",
    "Hyper.app",
    "1Password.app",
    "1Password 7.app",
    "Bitwarden.app",
    "KeePassXC.app",
    "Dashlane.app",
    "Enpass.app",
    "LastPass.app",
    "Passwords.app",
    "Keychain Access.app",
    "Terax.app",
];

/// Display names refused before a bundle is resolved. `Terminal` is enough
/// to refuse `launch Terminal` without scanning the disk.
const DENIED_DISPLAY_NAMES: &[&str] = &[
    "Terminal",
    "iTerm",
    "iTerm2",
    "Warp",
    "Alacritty",
    "kitty",
    "Ghostty",
    "WezTerm",
    "Hyper",
    "1Password",
    "1Password 7",
    "Bitwarden",
    "KeePassXC",
    "Dashlane",
    "Enpass",
    "LastPass",
    "Passwords",
    "Keychain Access",
    "Windows Terminal",
    "Terax",
];

/// Why a built-in rule refused an app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denial {
    /// `bundle`, `path`, `name`, or `system-settings`.
    pub source: &'static str,
    /// Human-readable explanation. Does not include typed text or URLs.
    pub message: String,
}

/// Refuse `selector` before it is resolved, when the selector itself is enough.
pub fn selector_denial(selector: &AppSelector) -> Option<Denial> {
    match selector {
        AppSelector::Pid(_) => None,
        AppSelector::BundleId(bundle_id) => bundle_denial(bundle_id).map(|message| Denial {
            source: "bundle",
            message,
        }),
        AppSelector::Name(name) => {
            if denied_display_name(name) {
                Some(Denial {
                    source: "name",
                    message: format!("built-in deny list refuses {name}"),
                })
            } else {
                None
            }
        }
        AppSelector::Path(path) => path_denial(path).map(|message| Denial {
            source: "path",
            message,
        }),
    }
}

/// Refuse a resolved app. System Settings is refused only when the targeted
/// window title or accessibility identifier mentions Privacy or Security.
pub fn identity_denial(
    bundle_id: Option<&str>,
    exe_path: Option<&Path>,
    window_title: Option<&str>,
    ax_identifier: Option<&str>,
) -> Option<Denial> {
    if let Some(bundle_id) = bundle_id {
        let exe_name = exe_path
            .and_then(|path| path.file_name())
            .and_then(|file| file.to_str());
        let bundle = exe_path
            .and_then(|path| path.components().next_back())
            .map(|component| component.as_os_str().to_string_lossy().to_string())
            .filter(|text| text.ends_with(".app"));
        if !exempts_identity(Some(bundle_id), exe_name, bundle.as_deref()) {
            if let Some(message) = bundle_denial(bundle_id) {
                return Some(Denial {
                    source: "bundle",
                    message,
                });
            }
        }
        if is_system_settings(bundle_id) && privacy_or_security(window_title, ax_identifier) {
            return Some(Denial {
                source: "system-settings",
                message: "built-in deny list refuses System Settings while the targeted window title or identifier contains Privacy or Security".into(),
            });
        }
    }
    if let Some(path) = exe_path {
        let exe_name = path.file_name().and_then(|file| file.to_str());
        let bundle = path
            .components()
            .next_back()
            .map(|component| component.as_os_str().to_string_lossy().to_string())
            .filter(|text| text.ends_with(".app"));
        if exempts_identity(bundle_id, exe_name, bundle.as_deref()) {
            return None;
        }
        if let Some(message) = path_denial(path) {
            return Some(Denial {
                source: "path",
                message,
            });
        }
    }
    None
}

/// Map a denial to [`ComputerError::PolicyDenied`].
pub fn deny_or_allow(denial: Option<Denial>) -> Result<(), ComputerError> {
    match denial {
        None => Ok(()),
        Some(denial) => Err(ComputerError::PolicyDenied(denial.message)),
    }
}

/// `loginwindow`, `WindowServer`, and this process cannot be quit.
pub fn quit_is_protected(
    pid: i32,
    current_pid: i32,
    bundle_id: Option<&str>,
    name: &str,
    path: Option<&str>,
) -> bool {
    if pid == current_pid {
        return true;
    }
    if bundle_id.is_some_and(|id| id.eq_ignore_ascii_case("com.apple.loginwindow")) {
        return true;
    }
    let name = name.trim();
    if name.eq_ignore_ascii_case("loginwindow") || name.eq_ignore_ascii_case("WindowServer") {
        return true;
    }
    path.is_some_and(|path| {
        Path::new(path)
            .file_name()
            .and_then(|file| file.to_str())
            .is_some_and(|file| {
                file.eq_ignore_ascii_case("loginwindow")
                    || file.eq_ignore_ascii_case("WindowServer")
            })
    })
}

fn bundle_denial(bundle_id: &str) -> Option<String> {
    if is_exempt(bundle_id) {
        return None;
    }
    DENIED_BUNDLE_IDS
        .iter()
        .find(|id| id.eq_ignore_ascii_case(bundle_id))
        .map(|id| format!("built-in deny list refuses {id}"))
}

fn path_denial(path: &Path) -> Option<String> {
    if let Some(file) = path.file_name().and_then(|file| file.to_str()) {
        if DENIED_EXE_NAMES
            .iter()
            .any(|name| name.eq_ignore_ascii_case(file))
        {
            return Some(format!(
                "built-in deny list refuses executable {}",
                path.display()
            ));
        }
    }
    for component in path.components() {
        let text = component.as_os_str().to_string_lossy();
        if DENIED_APP_BUNDLES
            .iter()
            .any(|name| text.eq_ignore_ascii_case(name))
        {
            return Some(format!(
                "built-in deny list refuses path {}",
                path.display()
            ));
        }
    }
    None
}

fn denied_display_name(name: &str) -> bool {
    let name = name.trim();
    if is_exempt(name) {
        return false;
    }
    DENIED_DISPLAY_NAMES
        .iter()
        .any(|denied| denied.eq_ignore_ascii_case(name))
}

fn is_system_settings(bundle_id: &str) -> bool {
    SYSTEM_SETTINGS_BUNDLES
        .iter()
        .any(|id| id.eq_ignore_ascii_case(bundle_id))
}

fn privacy_or_security(window_title: Option<&str>, ax_identifier: Option<&str>) -> bool {
    [window_title, ax_identifier]
        .into_iter()
        .flatten()
        .any(|text| {
            let lower = text.to_ascii_lowercase();
            lower.contains("privacy") || lower.contains("security")
        })
}

/// Marker stored in `kCGEventSourceUserData` (`CGEventField::EventSourceUserData`, value 42).
///
/// P7 posts synthetic events with this value. The listen-only tap ignores them.
/// Field 43 is `kCGEventSourceUserID` and is not the marker.
pub const SYNTHETIC_EVENT_USER_DATA: i64 = 0x00C0_5EC0_0DE1;

/// True when a tap event was posted by this process and must not pause.
pub fn event_is_synthetic(user_data: i64) -> bool {
    user_data == SYNTHETIC_EVENT_USER_DATA
}

const BROWSER_BUNDLES: &[&str] = &[
    "com.apple.Safari",
    "com.google.Chrome",
    "com.google.Chrome.canary",
    "org.chromium.Chromium",
    "com.microsoft.edgemac",
    "com.microsoft.Edge",
    "com.brave.Browser",
    "company.thebrowser.Browser",
    "org.mozilla.firefox",
];

const BROWSER_EXES: &[&str] = &[
    "Safari",
    "Google Chrome",
    "Chromium",
    "chrome",
    "chrome.exe",
    "chromium",
    "msedge",
    "msedge.exe",
    "Microsoft Edge",
    "brave",
    "Brave Browser",
    "Arc",
    "firefox",
    "firefox.exe",
];

/// How [`decide`] classifies an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyAction {
    /// `policy_check`. The app verdict is separate.
    Inspect,
    /// Always allowed: wait, doctor, displays, list, pause, resume.
    Always,
    /// Read-only maintenance that does not name a denied app.
    Benign,
    /// Tree, screenshot, or get-state. A denied target is refused.
    Observe,
    /// Changes the desktop.
    Mutate,
    /// Quit, including force quit. Allowed apps still need confirmation.
    Quit,
    /// A key combination.
    Key {
        /// Combo text such as `cmd+q`.
        combo: String,
    },
    /// `clipboard_set` (P8). Classified here so that phase can call [`decide`].
    ClipboardSet,
    /// `apple-event` and `shortcuts run`. Arbitrary script or workflow.
    Script,
}

/// Allow, refuse, or ask the confirm hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The action may run.
    Allow {
        /// `builtin`, `admin`, `user`, or `default`.
        source: &'static str,
    },
    /// The action must not run.
    Deny {
        /// `builtin`, `admin`, or `user`.
        source: &'static str,
        /// Human-readable explanation. Does not include typed text or URLs.
        message: String,
    },
    /// A sensitive action. Run the confirm hook, or return `confirm_required`.
    Confirm {
        /// Why confirmation is required.
        reason: String,
    },
}

/// Resolved target passed to [`decide`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Subject {
    /// Localized name.
    pub name: Option<String>,
    /// Bundle id.
    pub bundle_id: Option<String>,
    /// Executable path. [`decide`] canonicalizes it.
    pub path: Option<String>,
    /// Process id.
    pub pid: Option<i32>,
    /// Window id when one was targeted.
    pub window_id: Option<u32>,
    /// Targeted window title. Used for the System Settings privacy rule.
    pub window_title: Option<String>,
    /// Accessibility identifier of the targeted window.
    pub ax_identifier: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    effect: RuleEffect,
    bundle_id: Option<String>,
    path: Option<String>,
    name: Option<String>,
    source: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleEffect {
    Allow,
    Deny,
}

/// Merged admin and user policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedPolicy {
    user_default_deny: bool,
    admin_default_deny: bool,
    rules: Vec<Rule>,
    blocked_urls: Vec<String>,
    user_confirm: Option<String>,
    admin_confirm: Option<String>,
    timeout_secs: u64,
    /// Built-in deny entries the user policy exempts.
    builtin_exempt: Vec<String>,
    /// True only when a loaded file sets the key. A missing file stays false.
    pub pause_on_user_input: bool,
    /// `[security] secure_focus`. Default deny.
    secure_focus: FocusDecision,
    /// `[security] unreadable_focus`. Default warn.
    unreadable_focus: FocusDecision,
    /// `[audit] enabled`. Default true.
    audit_enabled: bool,
    /// `[audit] max_bytes`. Default 10 MiB.
    audit_max_bytes: u64,
    /// `[audit] keep`. Default 1, capped at 5.
    audit_keep: u32,
    /// `[window] clamp`. Default true.
    window_clamp: bool,
    /// Admin already set the security section, so the user file cannot relax it.
    security_admin: bool,
    /// Admin already set the audit section.
    audit_admin: bool,
    /// Admin already set the window section.
    window_admin: bool,
}

/// Direction for a security toggle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusDecision {
    /// Refuse the action.
    Deny,
    /// Proceed with a warning.
    Warn,
}

impl FocusDecision {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "deny" => Some(Self::Deny),
            "warn" => Some(Self::Warn),
            _ => None,
        }
    }
}

impl Default for LoadedPolicy {
    fn default() -> Self {
        Self {
            user_default_deny: false,
            admin_default_deny: false,
            rules: Vec::new(),
            blocked_urls: Vec::new(),
            user_confirm: None,
            admin_confirm: None,
            timeout_secs: 30,
            builtin_exempt: Vec::new(),
            pause_on_user_input: false,
            secure_focus: FocusDecision::Deny,
            unreadable_focus: FocusDecision::Warn,
            audit_enabled: true,
            audit_max_bytes: crate::audit::DEFAULT_MAX_BYTES,
            audit_keep: 1,
            window_clamp: true,
            security_admin: false,
            audit_admin: false,
            window_admin: false,
        }
    }
}

impl LoadedPolicy {
    /// Patterns from the user and admin files.
    pub fn blocked_urls(&self) -> &[String] {
        &self.blocked_urls
    }

    /// Built-in deny entries the user policy exempts.
    pub fn builtin_exempt(&self) -> &[String] {
        &self.builtin_exempt
    }

    /// What to do when the focused element is a secure field.
    pub fn secure_focus(&self) -> FocusDecision {
        self.secure_focus
    }

    /// What to do when the focused element cannot be read.
    pub fn unreadable_focus(&self) -> FocusDecision {
        self.unreadable_focus
    }

    /// Whether the audit log is enabled.
    pub fn audit_enabled(&self) -> bool {
        self.audit_enabled
    }

    /// Audit rotation threshold in bytes.
    pub fn audit_max_bytes(&self) -> u64 {
        self.audit_max_bytes
    }

    /// Audit backups to keep.
    pub fn audit_keep(&self) -> u32 {
        self.audit_keep
    }

    /// Whether window actions clamp to a display's visible frame.
    pub fn window_clamp(&self) -> bool {
        self.window_clamp
    }

    /// Timeout for the confirm hook. Capped at 120 seconds.
    pub fn timeout_secs(&self) -> u64 {
        self.timeout_secs.clamp(1, 120)
    }
}

/// Built-in deny, then admin, then user. Deny wins over allow.
pub fn decide(policy: &LoadedPolicy, target: &Subject, action: &PolicyAction) -> Decision {
    if matches!(
        action,
        PolicyAction::Always | PolicyAction::Benign | PolicyAction::Inspect
    ) {
        return Decision::Allow { source: "default" };
    }
    let verdict = app_verdict(policy, target);
    if let Decision::Deny { .. } = &verdict {
        return verdict;
    }
    if unscoped_target(target)
        && matches!(
            action,
            PolicyAction::Mutate
                | PolicyAction::Quit
                | PolicyAction::Key { .. }
                | PolicyAction::ClipboardSet
        )
        && (policy.admin_default_deny || policy.user_default_deny)
    {
        let source = if policy.admin_default_deny {
            "admin"
        } else {
            "user"
        };
        return Decision::Deny {
            source,
            message: format!("{source} policy default is deny and no app was named"),
        };
    }
    if matches!(action, PolicyAction::Quit) && quit_protected(target) {
        return Decision::Deny {
            source: "builtin",
            message: "refusing to quit loginwindow, WindowServer, or this process".into(),
        };
    }
    if let Some(reason) = confirm_reason(action) {
        return Decision::Confirm { reason };
    }
    verdict
}

/// Allow or deny for `policy check`. Never returns [`Decision::Confirm`].
pub fn app_verdict(policy: &LoadedPolicy, target: &Subject) -> Decision {
    let canonical = target
        .path
        .as_deref()
        .map(|path| crate::target::canonical_exe_path(Path::new(path)));
    if let Some(denial) = identity_denial(
        target.bundle_id.as_deref(),
        canonical.as_deref(),
        target.window_title.as_deref(),
        target.ax_identifier.as_deref(),
    ) {
        return Decision::Deny {
            source: "builtin",
            message: denial.message,
        };
    }
    if target.bundle_id.is_none() && canonical.is_none() {
        if let Some(name) = target.name.as_deref() {
            if denied_display_name(name) {
                return Decision::Deny {
                    source: "builtin",
                    message: format!("built-in deny list refuses {name}"),
                };
            }
        }
    }
    let subject_path = canonical.unwrap_or_default();
    if policy.rules.iter().any(|rule| {
        rule.source == "admin"
            && rule.effect == RuleEffect::Deny
            && deny_matches(rule, target, &subject_path)
    }) {
        return Decision::Deny {
            source: "admin",
            message: "admin policy denies this app".into(),
        };
    }
    if policy.rules.iter().any(|rule| {
        rule.source == "user"
            && rule.effect == RuleEffect::Deny
            && deny_matches(rule, target, &subject_path)
    }) {
        return Decision::Deny {
            source: "user",
            message: "user policy denies this app".into(),
        };
    }
    if let Some(message) = allow_path_mismatch(policy, target, &subject_path) {
        return Decision::Deny {
            source: "user",
            message,
        };
    }
    // An empty subject is the desktop or an action that named no app.
    // Default deny applies to a named app. Unscoped mutating actions are
    // refused in `decide`. A user allow never overrides an admin default deny.
    let named = !unscoped_target(target);
    if policy.admin_default_deny && named {
        return Decision::Deny {
            source: "admin",
            message: "admin policy default is deny".into(),
        };
    }
    if user_allow_matches(policy, target, &subject_path) {
        return Decision::Allow { source: "user" };
    }
    if policy.user_default_deny && named {
        return Decision::Deny {
            source: "user",
            message: "user policy default is deny".into(),
        };
    }
    Decision::Allow { source: "default" }
}

fn unscoped_target(target: &Subject) -> bool {
    target.name.is_none()
        && target.bundle_id.is_none()
        && target.path.is_none()
        && target.pid.is_none()
}

fn quit_protected(target: &Subject) -> bool {
    let Some(pid) = target.pid else {
        return false;
    };
    quit_is_protected(
        pid,
        std::process::id() as i32,
        target.bundle_id.as_deref(),
        target.name.as_deref().unwrap_or(""),
        target.path.as_deref(),
    )
}

fn confirm_reason(action: &PolicyAction) -> Option<String> {
    if matches!(action, PolicyAction::Script) {
        return Some(
            "runs an arbitrary Apple event script or Shortcuts workflow; configure a confirm hook to allow it"
                .into(),
        );
    }
    match action {
        PolicyAction::Quit => Some("quit".into()),
        PolicyAction::ClipboardSet => Some("clipboard_set".into()),
        PolicyAction::Key { combo } => {
            sensitive_combo(combo).map(|kind| format!("key combination {combo}: {}", kind.reason()))
        }
        _ => None,
    }
}

fn deny_matches(rule: &Rule, target: &Subject, canonical: &Path) -> bool {
    if let Some(bundle) = &rule.bundle_id {
        if target
            .bundle_id
            .as_ref()
            .is_some_and(|id| id.eq_ignore_ascii_case(bundle))
        {
            return true;
        }
    }
    if let Some(path) = &rule.path {
        if path_matches(canonical, path) {
            return true;
        }
    }
    if let Some(name) = &rule.name {
        if target
            .name
            .as_ref()
            .is_some_and(|value| value.eq_ignore_ascii_case(name))
        {
            return true;
        }
    }
    false
}

fn user_allow_matches(policy: &LoadedPolicy, target: &Subject, canonical: &Path) -> bool {
    policy.rules.iter().any(|rule| {
        rule.source == "user"
            && rule.effect == RuleEffect::Allow
            && allow_matches(rule, target, canonical)
    })
}

fn allow_matches(rule: &Rule, target: &Subject, canonical: &Path) -> bool {
    let mut saw = false;
    if let Some(bundle) = &rule.bundle_id {
        saw = true;
        if !target
            .bundle_id
            .as_ref()
            .is_some_and(|id| id.eq_ignore_ascii_case(bundle))
        {
            return false;
        }
    }
    if let Some(path) = &rule.path {
        saw = true;
        if !path_matches(canonical, path) {
            return false;
        }
    }
    if let Some(name) = &rule.name {
        saw = true;
        if !target
            .name
            .as_ref()
            .is_some_and(|value| value.eq_ignore_ascii_case(name))
        {
            return false;
        }
    }
    saw
}

fn allow_path_mismatch(
    policy: &LoadedPolicy,
    target: &Subject,
    canonical: &Path,
) -> Option<String> {
    let bundle = target.bundle_id.as_deref()?;
    let constrained: Vec<&Rule> = policy
        .rules
        .iter()
        .filter(|rule| {
            rule.effect == RuleEffect::Allow
                && rule.source == "user"
                && rule.path.is_some()
                && rule
                    .bundle_id
                    .as_ref()
                    .is_some_and(|id| id.eq_ignore_ascii_case(bundle))
        })
        .collect();
    if constrained.is_empty() {
        return None;
    }
    if constrained
        .iter()
        .any(|rule| path_matches(canonical, rule.path.as_deref().unwrap_or("")))
    {
        return None;
    }
    if policy.rules.iter().any(|rule| {
        rule.source == "user"
            && rule.effect == RuleEffect::Allow
            && rule.path.is_none()
            && rule
                .bundle_id
                .as_ref()
                .is_some_and(|id| id.eq_ignore_ascii_case(bundle))
    }) {
        return None;
    }
    Some("allow.path does not match the canonical executable path".into())
}

fn path_matches(canonical: &Path, rule_path: &str) -> bool {
    if canonical.as_os_str().is_empty() || rule_path.is_empty() {
        return false;
    }
    let rule = Path::new(rule_path);
    if canonical == rule {
        return true;
    }
    let subject = normalize_path(canonical);
    let allow = normalize_path(rule);
    if subject.eq_ignore_ascii_case(&allow) {
        return true;
    }
    let with_sep = if allow.ends_with('/') {
        allow.clone()
    } else {
        format!("{allow}/")
    };
    subject
        .to_ascii_lowercase()
        .starts_with(&with_sep.to_ascii_lowercase())
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Load the user file at `explicit` (or the default path) and merge the admin file.
pub fn load_policy(explicit: Option<&Path>) -> Result<LoadedPolicy, ComputerError> {
    let user = user_policy_file(explicit)?;
    let admin = admin_policy_file();
    load_paths(user.as_deref(), admin.as_deref())
}

/// Load two already chosen files. `None` means that file is absent.
pub fn load_paths(
    user: Option<&Path>,
    admin: Option<&Path>,
) -> Result<LoadedPolicy, ComputerError> {
    let mut policy = LoadedPolicy::default();
    if let Some(path) = admin {
        merge_file(&mut policy, path, PolicyOrigin::Admin)?;
    }
    if let Some(path) = user {
        merge_file(&mut policy, path, PolicyOrigin::User)?;
    }
    policy.timeout_secs = policy.timeout_secs.min(120);
    Ok(policy)
}

#[derive(Clone, Copy)]
enum PolicyOrigin {
    User,
    Admin,
}

fn user_policy_file(explicit: Option<&Path>) -> Result<Option<std::path::PathBuf>, ComputerError> {
    if let Some(path) = explicit {
        if !path.is_file() {
            return Err(ComputerError::InvalidArguments(format!(
                "policy file not found: {}",
                path.display()
            )));
        }
        return Ok(Some(path.to_path_buf()));
    }
    let Some(base) = user_config_base() else {
        return Ok(None);
    };
    let path = base.join("computer-use").join("policy.toml");
    if path.is_file() {
        Ok(Some(path))
    } else {
        Ok(None)
    }
}

fn user_config_base() -> Option<std::path::PathBuf> {
    // Explicit `--policy` paths still load. Ambient config must not, in tests.
    ambient_user_config_base()
}

#[cfg(test)]
fn ambient_user_config_base() -> Option<std::path::PathBuf> {
    None
}

#[cfg(not(test))]
fn ambient_user_config_base() -> Option<std::path::PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Some(std::path::PathBuf::from(xdg));
        }
    }
    dirs::home_dir().map(|home| home.join(".config"))
}

fn admin_policy_file() -> Option<std::path::PathBuf> {
    ambient_admin_policy_file()
}

#[cfg(test)]
fn ambient_admin_policy_file() -> Option<std::path::PathBuf> {
    None
}

#[cfg(not(test))]
fn ambient_admin_policy_file() -> Option<std::path::PathBuf> {
    if let Some(overridden) = std::env::var_os("COMPUTER_USE_ADMIN_POLICY") {
        let path = std::path::PathBuf::from(overridden);
        return path.is_file().then_some(path);
    }
    let path = default_admin_path();
    path.is_file().then_some(path)
}

#[cfg(all(not(test), windows))]
fn default_admin_path() -> std::path::PathBuf {
    let base = std::env::var_os("ProgramData")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\ProgramData"));
    base.join("computer-use").join("policy.toml")
}

#[cfg(all(not(test), not(windows)))]
fn default_admin_path() -> std::path::PathBuf {
    std::path::PathBuf::from("/etc/computer-use/policy.toml")
}

fn merge_file(
    policy: &mut LoadedPolicy,
    path: &Path,
    origin: PolicyOrigin,
) -> Result<(), ComputerError> {
    let text = std::fs::read_to_string(path).map_err(|error| policy_io(origin, path, error))?;
    if text.trim().is_empty() {
        return Ok(());
    }
    let parsed: PolicyFile =
        toml::from_str(&text).map_err(|error| parse_error(origin, path, &text, &error))?;
    if parsed.version.unwrap_or(0) != 1 {
        return Err(origin_error(
            origin,
            path,
            format!(
                "line {}: policy version must be 1",
                line_of_key(&text, "version")
            ),
        ));
    }
    let default_deny = match parsed.default.as_deref() {
        None => false,
        Some("allow") => false,
        Some("deny") => true,
        Some(other) => {
            return Err(origin_error(
                origin,
                path,
                format!(
                    "line {}: default must be allow or deny (got {other})",
                    line_of_key(&text, "default")
                ),
            ))
        }
    };
    if parsed.pause_on_user_input.unwrap_or(false) {
        policy.pause_on_user_input = true;
    }
    let source = match origin {
        PolicyOrigin::User => {
            policy.user_default_deny = default_deny;
            if !policy.security_admin {
                apply_security(policy, parsed.security.as_ref(), origin, path, &text)?;
            }
            if !policy.audit_admin {
                apply_audit(policy, parsed.audit.as_ref(), origin, path, &text)?;
            }
            if !policy.window_admin {
                apply_window(policy, parsed.window.as_ref(), origin, path, &text)?;
            }
            if let Some(builtin) = parsed.builtin_deny.as_ref() {
                for entry in &builtin.exempt {
                    let entry = entry.trim();
                    if !entry.is_empty() && !policy.builtin_exempt.iter().any(|item| item == entry)
                    {
                        policy.builtin_exempt.push(entry.to_string());
                    }
                }
            }
            if let Some(confirm) = parsed.confirm.as_ref() {
                if let Some(command) = confirm
                    .command
                    .clone()
                    .filter(|text| !text.trim().is_empty())
                {
                    policy.user_confirm = Some(command);
                }
                if let Some(timeout) = confirm.timeout_secs {
                    policy.timeout_secs = timeout.clamp(1, 120);
                }
            }
            "user"
        }
        PolicyOrigin::Admin => {
            policy.admin_default_deny = default_deny;
            apply_security(policy, parsed.security.as_ref(), origin, path, &text)?;
            policy.security_admin = parsed.security.is_some();
            apply_audit(policy, parsed.audit.as_ref(), origin, path, &text)?;
            policy.audit_admin = parsed.audit.is_some();
            apply_window(policy, parsed.window.as_ref(), origin, path, &text)?;
            policy.window_admin = parsed.window.is_some();
            if let Some(confirm) = parsed.confirm.as_ref() {
                if let Some(command) = confirm
                    .command
                    .clone()
                    .filter(|text| !text.trim().is_empty())
                {
                    policy.admin_confirm = Some(command);
                }
                if policy.user_confirm.is_none() {
                    if let Some(timeout) = confirm.timeout_secs {
                        policy.timeout_secs = timeout.clamp(1, 120);
                    }
                }
            }
            "admin"
        }
    };
    for entry in parsed.deny {
        policy.rules.push(
            rule_from(entry, RuleEffect::Deny, source)
                .map_err(|detail| origin_error(origin, path, detail))?,
        );
    }
    if matches!(origin, PolicyOrigin::User) {
        for entry in parsed.allow {
            policy.rules.push(
                rule_from(entry, RuleEffect::Allow, source)
                    .map_err(|detail| origin_error(origin, path, detail))?,
            );
        }
    }
    if let Some(browser) = parsed.browser {
        policy.blocked_urls.extend(browser.blocked_urls);
    }
    Ok(())
}

fn apply_security(
    policy: &mut LoadedPolicy,
    section: Option<&SecuritySection>,
    origin: PolicyOrigin,
    path: &Path,
    text: &str,
) -> Result<(), ComputerError> {
    let Some(section) = section else {
        return Ok(());
    };
    if let Some(value) = section.secure_focus.as_deref() {
        policy.secure_focus = FocusDecision::parse(value).ok_or_else(|| {
            origin_error(
                origin,
                path,
                format!(
                    "line {}: security.secure_focus must be deny or warn (got {value})",
                    line_of_key(text, "secure_focus")
                ),
            )
        })?;
    }
    if let Some(value) = section.unreadable_focus.as_deref() {
        policy.unreadable_focus = FocusDecision::parse(value).ok_or_else(|| {
            origin_error(
                origin,
                path,
                format!(
                    "line {}: security.unreadable_focus must be deny or warn (got {value})",
                    line_of_key(text, "unreadable_focus")
                ),
            )
        })?;
    }
    Ok(())
}

fn apply_audit(
    policy: &mut LoadedPolicy,
    section: Option<&AuditSection>,
    origin: PolicyOrigin,
    path: &Path,
    text: &str,
) -> Result<(), ComputerError> {
    let Some(section) = section else {
        return Ok(());
    };
    if let Some(enabled) = section.enabled {
        policy.audit_enabled = enabled;
    }
    if let Some(max_bytes) = section.max_bytes {
        if max_bytes < 1024 {
            return Err(origin_error(
                origin,
                path,
                format!(
                    "line {}: audit.max_bytes must be at least 1024",
                    line_of_key(text, "max_bytes")
                ),
            ));
        }
        policy.audit_max_bytes = max_bytes;
    }
    if let Some(keep) = section.keep {
        policy.audit_keep = keep.clamp(1, 5);
    }
    Ok(())
}

fn apply_window(
    policy: &mut LoadedPolicy,
    section: Option<&WindowSection>,
    _origin: PolicyOrigin,
    _path: &Path,
    _text: &str,
) -> Result<(), ComputerError> {
    let Some(section) = section else {
        return Ok(());
    };
    if let Some(clamp) = section.clamp {
        policy.window_clamp = clamp;
    }
    Ok(())
}

fn rule_from(entry: PolicyEntry, effect: RuleEffect, source: &'static str) -> Result<Rule, String> {
    if entry.bundle_id.is_none() && entry.path.is_none() && entry.name.is_none() {
        return Err("policy allow and deny entries need bundle_id, path, or name".into());
    }
    let path = entry.path.map(|path| {
        crate::target::canonical_exe_path(Path::new(&path))
            .to_string_lossy()
            .into_owned()
    });
    Ok(Rule {
        effect,
        bundle_id: entry.bundle_id,
        path,
        name: entry.name,
        source,
    })
}

fn policy_io(origin: PolicyOrigin, path: &Path, error: std::io::Error) -> ComputerError {
    origin_error(origin, path, error.to_string())
}

fn line_of_key(text: &str, key: &str) -> usize {
    text.lines()
        .position(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with(key) && trimmed[key.len()..].trim_start().starts_with('=')
        })
        .map(|index| index + 1)
        .unwrap_or(1)
}

fn parse_error(
    origin: PolicyOrigin,
    path: &Path,
    text: &str,
    error: &toml::de::Error,
) -> ComputerError {
    let line = error
        .span()
        .map(|span| {
            text[..span.start.min(text.len())]
                .chars()
                .filter(|ch| *ch == '\n')
                .count()
                + 1
        })
        .or_else(|| line_from_display(&error.to_string()))
        .unwrap_or(1);
    origin_error(origin, path, format!("line {line}: {error}"))
}

fn line_from_display(text: &str) -> Option<usize> {
    let marker = "at line ";
    let start = text.find(marker)? + marker.len();
    text[start..]
        .split(|ch: char| !ch.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

fn origin_error(origin: PolicyOrigin, path: &Path, detail: String) -> ComputerError {
    match origin {
        PolicyOrigin::User => {
            ComputerError::InvalidArguments(format!("{}: {detail}", path.display()))
        }
        PolicyOrigin::Admin => ComputerError::PolicyDenied(format!("{}: {detail}", path.display())),
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFile {
    #[serde(default)]
    version: Option<u32>,
    #[serde(default)]
    default: Option<String>,
    #[serde(default)]
    pause_on_user_input: Option<bool>,
    #[serde(default)]
    allow: Vec<PolicyEntry>,
    #[serde(default)]
    deny: Vec<PolicyEntry>,
    #[serde(default)]
    confirm: Option<ConfirmSection>,
    #[serde(default)]
    browser: Option<BrowserSection>,
    #[serde(default)]
    builtin_deny: Option<BuiltinDenySection>,
    #[serde(default)]
    security: Option<SecuritySection>,
    #[serde(default)]
    audit: Option<AuditSection>,
    #[serde(default)]
    window: Option<WindowSection>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SecuritySection {
    #[serde(default)]
    secure_focus: Option<String>,
    #[serde(default)]
    unreadable_focus: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AuditSection {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    max_bytes: Option<u64>,
    #[serde(default)]
    keep: Option<u32>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WindowSection {
    #[serde(default)]
    clamp: Option<bool>,
}

/// Exemptions from the built-in deny list. Only the user policy file may set
/// this; an admin file that sets it is ignored.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BuiltinDenySection {
    /// Bundle ids, executable names, or `.app` bundle names to exempt.
    #[serde(default)]
    exempt: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyEntry {
    #[serde(default)]
    bundle_id: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfirmSection {
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserSection {
    #[serde(default)]
    blocked_urls: Vec<String>,
}

/// Program from the flag, then `COMPUTER_USE_CONFIRM_CMD`, then user policy, then admin.
///
/// Unit tests do not read the environment variable. The CLI process does.
pub fn confirm_program(policy: &LoadedPolicy, flag: Option<&str>) -> Option<String> {
    if let Some(flag) = flag.map(str::trim).filter(|text| !text.is_empty()) {
        return Some(flag.to_string());
    }
    #[cfg(not(test))]
    if let Ok(env) = std::env::var("COMPUTER_USE_CONFIRM_CMD") {
        if !env.trim().is_empty() {
            return Some(env);
        }
    }
    policy
        .user_confirm
        .clone()
        .or_else(|| policy.admin_confirm.clone())
}

/// JSON object written to the confirm hook's stdin.
#[derive(Debug, serde::Serialize)]
pub struct ConfirmDocument<'a> {
    /// Action name.
    pub action: &'a str,
    /// Target bundle id.
    pub bundle_id: Option<&'a str>,
    /// Target executable path.
    pub path: Option<&'a str>,
    /// Target pid.
    pub pid: Option<i32>,
    /// Target window id.
    pub window_id: Option<u32>,
    /// Element index.
    pub element: Option<u32>,
    /// Why this action is sensitive.
    pub reason: &'a str,
    /// Action-specific detail that does not contain the secret body: script
    /// bytes and hash for `apple-event`, the workflow name for `shortcuts`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

/// Spawn the hook. Exit 0 proceeds. Any other result is `confirm_denied`.
///
/// No configured command is `confirm_required` and the action does not run.
pub fn run_confirm(
    command: Option<&str>,
    timeout_secs: u64,
    document: &ConfirmDocument<'_>,
) -> Result<(), ComputerError> {
    let Some(command) = command.map(str::trim).filter(|text| !text.is_empty()) else {
        return Err(ComputerError::ConfirmRequired(format!(
            "{} requires a confirm hook ({})",
            document.action, document.reason
        )));
    };
    let (program, args) = split_command(command)?;
    let payload = serde_json::json!({
        "action": document.action,
        "app": {
            "bundle_id": document.bundle_id,
            "path": document.path,
        },
        "pid": document.pid,
        "window_id": document.window_id,
        "element": document.element,
        "reason": document.reason,
        "extra": document.extra,
    });
    let bytes = serde_json::to_vec(&payload).map_err(|error| {
        ComputerError::ExecutionFailed(format!("confirm document encode failed: {error}"))
    })?;
    let timeout = std::time::Duration::from_secs(timeout_secs.clamp(1, 120));
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    match crate::backend::run_with_stdin(&program, &arg_refs, &bytes, timeout) {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(ComputerError::ConfirmDenied(format!(
            "confirm hook exited {}",
            output
                .status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "without a status".into())
        ))),
        Err(error) => Err(ComputerError::ConfirmDenied(error)),
    }
}

/// Split a command into a program and arguments. Quotes may use `"` or `'`.
pub fn split_command(input: &str) -> Result<(String, Vec<String>), ComputerError> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    for ch in input.chars() {
        match quote {
            Some(mark) if ch == mark => quote = None,
            Some(_) => current.push(ch),
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch.is_whitespace() => {
                if any {
                    tokens.push(std::mem::take(&mut current));
                    any = false;
                }
            }
            None => {
                current.push(ch);
                any = true;
            }
        }
    }
    if quote.is_some() {
        return Err(ComputerError::InvalidArguments(
            "confirm command has an unmatched quote".into(),
        ));
    }
    if any {
        tokens.push(current);
    }
    let mut tokens = tokens.into_iter();
    let program = tokens
        .next()
        .ok_or_else(|| ComputerError::InvalidArguments("confirm command is empty".into()))?;
    Ok((program, tokens.collect()))
}

/// Click or `AXPress` titles that need confirmation.
pub fn sensitive_title(title: &str) -> bool {
    title
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .any(|word| {
            matches!(
                word.to_ascii_lowercase().as_str(),
                "delete"
                    | "erase"
                    | "pay"
                    | "purchase"
                    | "send"
                    | "submit"
                    | "trash"
                    | "discard"
                    | "uninstall"
            )
        })
}

/// Stable FNV-1a 64-bit hex hash. Used by the audit log and the confirm
/// document so a hook can identify a script without seeing its body.
pub fn script_hash(bytes: &[u8]) -> String {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

std::thread_local! {
    static WINDOW_CLAMP: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// Store `[window] clamp` from the loaded policy for this action.
pub fn set_window_clamp(value: bool) {
    WINDOW_CLAMP.with(|cell| cell.set(value));
}

/// Whether window moves and resizes clamp to a display's frame. Default true.
pub fn window_clamp() -> bool {
    WINDOW_CLAMP.with(|cell| cell.get())
}

/// A sensitive system shortcut that needs explicit confirmation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SensitiveCombo {
    /// Quit the frontmost app.
    Quit,
    /// Log out of the session.
    LogOut,
    /// Lock the screen.
    LockScreen,
    /// Force Quit the frontmost app.
    ForceQuit,
    /// Move the Finder selection to the trash.
    EmptyTrash,
}

impl SensitiveCombo {
    /// Stable token for the confirm document.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quit => "quit",
            Self::LogOut => "log_out",
            Self::LockScreen => "lock_screen",
            Self::ForceQuit => "force_quit",
            Self::EmptyTrash => "empty_trash",
        }
    }

    /// Human reason for the confirm hook.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Quit => "quits the frontmost application",
            Self::LogOut => "logs out of the session",
            Self::LockScreen => "locks the screen",
            Self::ForceQuit => "force quits the frontmost application",
            Self::EmptyTrash => "moves the selection to the trash",
        }
    }
}

/// Detect a sensitive system shortcut. Extra modifiers do not hide it:
/// `cmd+shift+q` is still a logout.
pub fn sensitive_combo(keys: &str) -> Option<SensitiveCombo> {
    let combos = crate::keys::parse_key_sequence(keys).ok()?;
    for combo in combos {
        use crate::types::Modifier;
        let key = combo.key.to_ascii_lowercase();
        let has = |modifier: Modifier| combo.modifiers.contains(&modifier);
        let command = has(Modifier::Command) || has(Modifier::Meta);
        let control = has(Modifier::Control);
        let alt = has(Modifier::Alt);
        let shift = has(Modifier::Shift);
        #[cfg(target_os = "windows")]
        let _ = shift;
        #[cfg(target_os = "linux")]
        let _ = shift;
        #[cfg(target_os = "macos")]
        {
            if key == "q" && command && shift {
                return Some(SensitiveCombo::LogOut);
            }
            if key == "q" && command && control {
                return Some(SensitiveCombo::LockScreen);
            }
            if key == "q" && command {
                return Some(SensitiveCombo::Quit);
            }
            if (key == "escape" || key == "esc") && command && alt {
                return Some(SensitiveCombo::ForceQuit);
            }
            if key == "delete" && command && shift {
                return Some(SensitiveCombo::EmptyTrash);
            }
        }
        #[cfg(target_os = "windows")]
        {
            let _ = control;
            if key == "f4" && alt {
                return Some(SensitiveCombo::Quit);
            }
            if key == "l" && command {
                return Some(SensitiveCombo::LockScreen);
            }
        }
        #[cfg(target_os = "linux")]
        {
            let _ = command;
            if key == "f4" && alt {
                return Some(SensitiveCombo::Quit);
            }
            if key == "l" && control && alt {
                return Some(SensitiveCombo::LockScreen);
            }
        }
    }
    None
}

/// True when the combination is one of the sensitive system shortcuts.
pub fn key_needs_confirm(keys: &str) -> bool {
    sensitive_combo(keys).is_some()
}

/// Safari, Chrome, Chromium, Edge, Brave, Arc, and Firefox.
pub fn is_browser(bundle_id: Option<&str>, path: Option<&Path>, name: Option<&str>) -> bool {
    if bundle_id.is_some_and(|id| {
        BROWSER_BUNDLES
            .iter()
            .any(|browser| browser.eq_ignore_ascii_case(id))
    }) {
        return true;
    }
    if let Some(path) = path {
        if let Some(file) = path.file_name().and_then(|file| file.to_str()) {
            if BROWSER_EXES
                .iter()
                .any(|exe| exe.eq_ignore_ascii_case(file))
            {
                return true;
            }
        }
    }
    name.is_some_and(|name| {
        BROWSER_EXES
            .iter()
            .any(|exe| exe.eq_ignore_ascii_case(name.trim()))
            || name.eq_ignore_ascii_case("Google Chrome")
            || name.eq_ignore_ascii_case("Safari")
            || name.eq_ignore_ascii_case("Firefox")
            || name.eq_ignore_ascii_case("Microsoft Edge")
            || name.eq_ignore_ascii_case("Brave Browser")
            || name.eq_ignore_ascii_case("Arc")
    })
}

/// First `http://`, `https://`, or `file://` token in `text`.
pub fn url_in_text(text: &str) -> Option<&str> {
    for token in text.split_whitespace() {
        let token =
            token.trim_matches(|ch: char| matches!(ch, '"' | '\'' | '<' | '>' | '(' | ')' | ','));
        if token.starts_with("https://")
            || token.starts_with("http://")
            || token.starts_with("file://")
        {
            return Some(token);
        }
    }
    None
}

/// Pattern that matches `url`, if any. `*` matches any span, including empty.
///
/// A pattern that ends in `/*` also matches the same URL with no trailing slash.
/// P10 calls this before CDP navigation.
pub fn url_blocked<'a>(url: &str, patterns: &'a [String]) -> Option<&'a str> {
    patterns.iter().find_map(|pattern| {
        if glob_match(pattern, url) || slash_star_origin(pattern, url) {
            Some(pattern.as_str())
        } else {
            None
        }
    })
}

fn slash_star_origin(pattern: &str, url: &str) -> bool {
    let Some(origin) = pattern.strip_suffix("/*") else {
        return false;
    };
    url == origin || url.starts_with(&format!("{origin}?"))
}

/// Match a `*` glob the same way `[browser].blocked_urls` does.
///
/// `*` matches any run of characters, including `/`. The comparison is
/// case-sensitive, like the policy check.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    if !text.starts_with(parts[0]) {
        return false;
    }
    let mut rest = &text[parts[0].len()..];
    for (index, part) in parts.iter().enumerate().skip(1) {
        if part.is_empty() {
            if index + 1 == parts.len() {
                return true;
            }
            continue;
        }
        if index + 1 == parts.len() {
            return rest.ends_with(part);
        }
        match rest.find(part) {
            Some(pos) => rest = &rest[pos + part.len()..],
            None => return false,
        }
    }
    true
}

std::thread_local! {
    static TEST_PAUSED: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Clears a test pause override when dropped.
pub struct PauseOverride {
    previous: Option<bool>,
}

impl Drop for PauseOverride {
    fn drop(&mut self) {
        let previous = self.previous.take();
        TEST_PAUSED.with(|slot| slot.set(previous));
    }
}

/// Force [`is_paused`] on this thread until the guard drops.
///
/// This does not create an event tap and does not set the process-wide flag.
pub fn override_paused(paused: bool) -> PauseOverride {
    let previous = TEST_PAUSED.with(|slot| slot.replace(Some(paused)));
    PauseOverride { previous }
}

/// Whether mutating actions must return `paused`.
///
/// The pause file is the cross-process source of truth. The atomic flag covers
/// this process when the file could not be written. Tests consult the
/// thread-local override first, then the file, and never the real home directory.
pub fn is_paused() -> bool {
    #[cfg(test)]
    if let Some(paused) = TEST_PAUSED.with(|slot| slot.get()) {
        return paused;
    }
    if pause_file_present() {
        return true;
    }
    paused_without_file()
}

#[cfg(test)]
fn paused_without_file() -> bool {
    false
}

#[cfg(not(test))]
fn paused_without_file() -> bool {
    if FILE_IO_OK.load(std::sync::atomic::Ordering::Acquire) {
        PAUSED.store(false, std::sync::atomic::Ordering::Release);
        false
    } else {
        PAUSED.load(std::sync::atomic::Ordering::Acquire)
    }
}

#[cfg(not(test))]
static PAUSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static FILE_IO_OK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Set or clear the pause flag and the `paused` file next to the audit log.
pub fn set_paused(paused: bool) -> Result<(), ComputerError> {
    #[cfg(not(test))]
    PAUSED.store(paused, std::sync::atomic::Ordering::Release);
    match write_pause_file(paused) {
        Ok(()) => {
            FILE_IO_OK.store(true, std::sync::atomic::Ordering::Release);
            Ok(())
        }
        Err(error) => {
            FILE_IO_OK.store(false, std::sync::atomic::Ordering::Release);
            Err(error)
        }
    }
}

fn pause_file_present() -> bool {
    crate::audit::state_dir()
        .map(|dir| dir.join("paused").is_file())
        .unwrap_or(false)
}

fn write_pause_file(paused: bool) -> Result<(), ComputerError> {
    let Some(dir) = crate::audit::state_dir() else {
        return Ok(());
    };
    std::fs::create_dir_all(&dir).map_err(ComputerError::Io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let path = dir.join("paused");
    if paused {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
            #[cfg(target_os = "linux")]
            options.custom_flags(0x20000);
            #[cfg(target_os = "macos")]
            options.custom_flags(0x0100);
        }
        options.open(&path).map_err(ComputerError::Io)?;
        Ok(())
    } else if path.exists() {
        std::fs::remove_file(&path).map_err(ComputerError::Io)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn terminal_bundle_id_and_fixture_path_are_denied() {
        let by_id = identity_denial(Some("com.apple.Terminal"), None, None, None).unwrap();
        assert_eq!(by_id.source, "bundle");
        assert!(by_id.message.contains("com.apple.Terminal"));

        let path = Path::new("/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal");
        let by_path = identity_denial(None, Some(path), None, None).unwrap();
        assert_eq!(by_path.source, "path");
        assert!(by_path.message.contains("Terminal.app"));

        let selector = AppSelector::BundleId("com.apple.Terminal".into());
        assert!(selector_denial(&selector).is_some());
        let named = AppSelector::Name(r#"Foo "Bar""#.into());
        assert!(selector_denial(&named).is_none());

        assert!(identity_denial(Some("app.crynta.terax"), None, None, None).is_some());
        assert!(selector_denial(&AppSelector::Name("Terax".into())).is_some());
        let terax = Path::new("/Applications/Terax.app/Contents/MacOS/Terax");
        assert!(identity_denial(None, Some(terax), None, None).is_some());
    }

    #[test]
    fn every_listed_bundle_and_exe_is_denied_and_notes_is_not() {
        for bundle_id in DENIED_BUNDLE_IDS {
            assert!(
                identity_denial(Some(bundle_id), None, None, None).is_some(),
                "{bundle_id}"
            );
        }
        for exe in DENIED_EXE_NAMES {
            let path = PathBuf::from("/usr/bin").join(exe);
            assert!(
                identity_denial(None, Some(&path), None, None).is_some(),
                "{exe}"
            );
        }
        assert!(identity_denial(
            Some("com.apple.Notes"),
            Some(Path::new(
                "/System/Applications/Notes.app/Contents/MacOS/Notes"
            )),
            None,
            None
        )
        .is_none());
        assert!(identity_denial(
            None,
            Some(Path::new("/usr/local/bin/TerminalHelper")),
            None,
            None
        )
        .is_none());
    }

    #[test]
    fn system_settings_privacy_window_is_denied_and_other_windows_are_not() {
        assert!(identity_denial(
            Some("com.apple.systempreferences"),
            Some(Path::new(
                "/System/Applications/System Settings.app/Contents/MacOS/System Settings"
            )),
            Some("Privacy & Security"),
            None,
        )
        .is_some());
        assert!(identity_denial(
            Some("com.apple.systempreferences"),
            None,
            Some("Displays"),
            Some("AXIdentifier"),
        )
        .is_none());
        assert!(identity_denial(Some("com.apple.systempreferences"), None, None, None).is_none());
        assert!(identity_denial(
            Some("com.apple.Settings"),
            None,
            Some("Privacy & Security"),
            None,
        )
        .is_some());
        assert!(
            identity_denial(Some("com.apple.Settings"), None, Some("Displays"), None).is_none()
        );
        assert!(identity_denial(
            Some("com.apple.systempreferences"),
            None,
            None,
            Some("Privacy_Security"),
        )
        .is_some());
    }

    #[test]
    fn powershell_cmd_and_wt_paths_are_denied() {
        for exe in ["powershell.exe", "cmd.exe", "wt.exe", "gnome-terminal"] {
            let path = PathBuf::from(r"C:\Windows\System32").join(exe);
            assert!(path_denial(&path).is_some(), "{exe}");
        }
    }

    #[test]
    fn quit_protects_loginwindow_windowserver_and_self() {
        assert!(quit_is_protected(9, 9, None, "computer-use", None));
        assert!(quit_is_protected(
            2,
            9,
            Some("com.apple.loginwindow"),
            "loginwindow",
            None
        ));
        assert!(quit_is_protected(3, 9, None, "WindowServer", None));
        assert!(!quit_is_protected(
            10,
            9,
            Some("com.apple.Notes"),
            "Notes",
            Some("/System/Applications/Notes.app/Contents/MacOS/Notes")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_a_terminal_executable_is_denied() {
        let dir = std::env::temp_dir().join(format!(
            "computer-use-policy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let real = dir.join("Terminal");
        fs::write(&real, b"fixture").unwrap();
        let link = dir.join("alias");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let canonical = fs::canonicalize(&link).unwrap();
        assert_eq!(
            canonical.file_name().and_then(|name| name.to_str()),
            Some("Terminal")
        );
        assert!(identity_denial(None, Some(&canonical), None, None).is_some());
        let user = dir.join("user.toml");
        std::fs::write(&user, "version = 1\n[[allow]]\npath = \"alias\"\n").unwrap();
        // The allow path is relative to the process, so point it at the symlink itself.
        let user_text = format!("version = 1\n[[allow]]\npath = \"{}\"\n", link.display());
        std::fs::write(&user, user_text).unwrap();
        let policy = load_paths(Some(&user), None).unwrap();
        let decision = decide(
            &policy,
            &Subject {
                path: Some(link.display().to_string()),
                ..Subject::default()
            },
            &PolicyAction::Mutate,
        );
        assert!(
            matches!(
                decision,
                Decision::Deny {
                    source: "builtin",
                    ..
                }
            ),
            "{decision:?}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    fn temp_dir(label: &str) -> PathBuf {
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

    fn subject_bundle(bundle: &str) -> Subject {
        Subject {
            bundle_id: Some(bundle.into()),
            ..Subject::default()
        }
    }

    #[test]
    fn default_deny_allows_only_listed_apps() {
        let dir = temp_dir("default-deny");
        let user = dir.join("user.toml");
        std::fs::write(
            &user,
            "version = 1\ndefault = \"deny\"\n\n[[allow]]\nbundle_id = \"com.apple.Notes\"\n",
        )
        .unwrap();
        let policy = load_paths(Some(&user), None).unwrap();
        assert!(matches!(
            app_verdict(&policy, &subject_bundle("com.apple.Notes")),
            Decision::Allow { source: "user" }
        ));
        assert!(matches!(
            app_verdict(&policy, &subject_bundle("com.apple.Finder")),
            Decision::Deny { source: "user", .. }
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn admin_deny_beats_user_allow_and_builtin_beats_user_allow() {
        let dir = temp_dir("merge");
        let admin = dir.join("admin.toml");
        let user = dir.join("user.toml");
        std::fs::write(
            &admin,
            "version = 1\n[[deny]]\nbundle_id = \"com.apple.Notes\"\n",
        )
        .unwrap();
        std::fs::write(
            &user,
            "version = 1\n[[allow]]\nbundle_id = \"com.apple.Notes\"\n[[allow]]\nbundle_id = \"com.apple.Terminal\"\n",
        )
        .unwrap();
        let policy = load_paths(Some(&user), Some(&admin)).unwrap();
        assert!(matches!(
            app_verdict(&policy, &subject_bundle("com.apple.Notes")),
            Decision::Deny {
                source: "admin",
                ..
            }
        ));
        assert!(matches!(
            app_verdict(&policy, &subject_bundle("com.apple.Terminal")),
            Decision::Deny {
                source: "builtin",
                ..
            }
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn glob_blocks_accounts_host_and_allows_example() {
        let patterns = vec!["https://accounts.example.com/*".to_string()];
        assert_eq!(
            url_blocked("https://accounts.example.com/login", &patterns),
            Some("https://accounts.example.com/*")
        );
        assert!(url_blocked("https://accounts.example.com/", &patterns).is_some());
        assert!(url_blocked("https://accounts.example.com", &patterns).is_some());
        assert!(url_blocked("https://example.com/", &patterns).is_none());
        assert!(url_blocked("https://example.com/accounts.example.com/", &patterns).is_none());
    }

    #[test]
    fn user_parse_error_names_the_line_and_admin_fails_closed() {
        let dir = temp_dir("parse");
        let user = dir.join("user.toml");
        std::fs::write(&user, "version = 1\n\ndefault = \"sometimes\"\n").unwrap();
        let error = load_paths(Some(&user), None).unwrap_err();
        assert_eq!(error.code(), "invalid_arguments");
        assert!(error.to_string().contains("line "), "{error}");
        let admin = dir.join("admin.toml");
        std::fs::write(&admin, "version = 1\nnope = true\n").unwrap();
        let error = load_paths(None, Some(&admin)).unwrap_err();
        assert_eq!(error.code(), "policy_denied");
        assert!(
            error.to_string().contains(&admin.display().to_string()),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn user_sections_set_security_audit_window_and_confirm() {
        let dir = temp_dir("sections");
        let user = dir.join("user.toml");
        std::fs::write(
            &user,
            r#"version = 1

[security]
secure_focus = "warn"
unreadable_focus = "deny"

[audit]
enabled = false
max_bytes = 4096
keep = 9

[window]
clamp = false

[confirm]
command = "/usr/local/bin/computer-use-confirm"
timeout_secs = 400

[builtin_deny]
exempt = ["app.crynta.terax", "  ", "app.crynta.terax"]
"#,
        )
        .unwrap();
        let policy = load_paths(Some(&user), None).unwrap();
        assert_eq!(policy.secure_focus(), FocusDecision::Warn);
        assert_eq!(policy.unreadable_focus(), FocusDecision::Deny);
        assert!(!policy.audit_enabled());
        assert_eq!(policy.audit_max_bytes(), 4096);
        assert_eq!(policy.audit_keep(), 5, "keep is clamped to 5");
        assert!(!policy.window_clamp());
        assert_eq!(policy.timeout_secs(), 120, "the hook timeout is capped");
        assert_eq!(policy.builtin_exempt(), ["app.crynta.terax"]);
        assert_eq!(
            confirm_program(&policy, None).as_deref(),
            Some("/usr/local/bin/computer-use-confirm")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn admin_sections_win_over_user_and_errors_name_the_line() {
        let dir = temp_dir("sections-admin");
        let admin = dir.join("admin.toml");
        let user = dir.join("user.toml");
        std::fs::write(
            &admin,
            "version = 1\n[security]\nsecure_focus = \"deny\"\n[window]\nclamp = true\n",
        )
        .unwrap();
        std::fs::write(
            &user,
            "version = 1\n[security]\nsecure_focus = \"warn\"\n[window]\nclamp = false\n",
        )
        .unwrap();
        let policy = load_paths(Some(&user), Some(&admin)).unwrap();
        assert_eq!(
            policy.secure_focus(),
            FocusDecision::Deny,
            "the admin file owns the section"
        );
        assert!(policy.window_clamp(), "the admin file owns the section");

        let bad = dir.join("bad.toml");
        std::fs::write(&bad, "version = 1\n[security]\nsecure_focus = \"maybe\"\n").unwrap();
        let error = load_paths(Some(&bad), None).unwrap_err();
        assert_eq!(error.code(), "invalid_arguments");
        assert!(error.to_string().contains("secure_focus"), "{error}");

        std::fs::write(&bad, "version = 1\n[audit]\nmax_bytes = 10\n").unwrap();
        let error = load_paths(Some(&bad), None).unwrap_err();
        assert!(error.to_string().contains("at least 1024"), "{error}");

        std::fs::write(&bad, "version = 2\n").unwrap();
        let error = load_paths(Some(&bad), None).unwrap_err();
        assert!(error.to_string().contains("version must be 1"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn builtin_exemption_parses_and_dedupes() {
        let dir = temp_dir("exempt");
        let user = dir.join("user.toml");
        std::fs::write(
            &user,
            "version = 1\n[builtin_deny]\nexempt = [\"app.crynta.terax\", \"  \", \"app.crynta.terax\"]\n",
        )
        .unwrap();
        let policy = load_paths(Some(&user), None).unwrap();
        assert_eq!(
            policy.builtin_exempt(),
            ["app.crynta.terax"],
            "blank entries are dropped and repeats collapse"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn url_in_text_only_accepts_http_and_file_tokens() {
        assert_eq!(
            url_in_text("open https://example.com/a now"),
            Some("https://example.com/a")
        );
        assert_eq!(
            url_in_text("(http://example.com)"),
            Some("http://example.com")
        );
        assert_eq!(url_in_text("file:///tmp/x"), Some("file:///tmp/x"));
        assert_eq!(url_in_text("ftp://example.com"), None);
        assert_eq!(url_in_text("no url here"), None);
    }

    #[test]
    fn browser_detection_matches_names_paths_and_bundle_ids() {
        assert!(is_browser(
            Some("com.google.Chrome"),
            None,
            Some("Google Chrome")
        ));
        assert!(is_browser(
            None,
            Some(Path::new("/Applications/Safari.app/Contents/MacOS/Safari")),
            None
        ));
        assert!(is_browser(None, None, Some("Firefox")));
        assert!(!is_browser(
            Some("com.apple.Notes"),
            Some(Path::new("/Applications/Notes.app/Contents/MacOS/Notes")),
            Some("Notes")
        ));
    }

    #[test]
    fn confirm_hook_exit_codes() {
        let dir = temp_dir("confirm");
        let document = ConfirmDocument {
            action: "quit",
            bundle_id: Some("com.apple.Notes"),
            path: None,
            pid: Some(10),
            window_id: None,
            element: None,
            reason: "quit",
            extra: None,
        };
        #[cfg(unix)]
        let (allow, deny) = {
            let allow = dir.join("allow.sh");
            let deny = dir.join("deny.sh");
            std::fs::write(&allow, "#!/bin/sh\ncat >/dev/null\nexit 0\n").unwrap();
            std::fs::write(&deny, "#!/bin/sh\ncat >/dev/null\nexit 1\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&allow, std::fs::Permissions::from_mode(0o700)).unwrap();
                std::fs::set_permissions(&deny, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            (allow.display().to_string(), deny.display().to_string())
        };
        #[cfg(windows)]
        let (allow, deny) = ("cmd /C exit 0".to_string(), "cmd /C exit 1".to_string());
        assert!(run_confirm(Some(&allow), 30, &document).is_ok());
        let denied = run_confirm(Some(&deny), 30, &document).unwrap_err();
        assert_eq!(denied.code(), "confirm_denied");
        let missing = run_confirm(None, 30, &document).unwrap_err();
        assert_eq!(missing.code(), "confirm_required");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sensitive_titles_and_quit_shortcuts() {
        assert!(sensitive_title("Delete Note"));
        assert!(sensitive_title("SEND"));
        assert!(!sensitive_title("Save"));
        #[cfg(target_os = "macos")]
        {
            assert_eq!(sensitive_combo("cmd+q"), Some(SensitiveCombo::Quit));
            assert_eq!(sensitive_combo("cmd+shift+q"), Some(SensitiveCombo::LogOut));
            assert_eq!(
                sensitive_combo("ctrl+cmd+q"),
                Some(SensitiveCombo::LockScreen)
            );
            assert_eq!(
                sensitive_combo("cmd+alt+esc"),
                Some(SensitiveCombo::ForceQuit)
            );
            assert_eq!(
                sensitive_combo("cmd+shift+delete"),
                Some(SensitiveCombo::EmptyTrash)
            );
            assert!(key_needs_confirm("meta+q"));
        }
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        {
            assert!(key_needs_confirm("alt+f4"));
        }
        assert!(!key_needs_confirm("cmd+s"));
        assert!(!event_is_synthetic(1));
        assert!(event_is_synthetic(SYNTHETIC_EVENT_USER_DATA));
    }
}

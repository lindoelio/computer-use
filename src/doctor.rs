//! Pure `doctor` report assembly.
//!
//! Platform backends fill a [`DoctorInput`]. This module decides `missing`,
//! `permissions_ok`, the Settings fixes, and the readable `text`. It does not
//! call Accessibility, CoreGraphics, `codesign`, or `open`.
//!
//! The response envelope's `ok` means the report was produced, so the process
//! exits 0. [`DoctorReport::permissions_ok`] is the permission aggregate.
//! Reusing `ok` for grants would turn a missing permission into a failed command.

use crate::target::{DoctorIdentity, DoctorReport, PermissionFix, ResponsibleApp};

/// Bundle id of `Computer Use.app`.
pub const HELPER_BUNDLE_ID: &str = "ai.lindoelio.computer-use";
/// Display name of the helper bundle.
pub const HELPER_APP_NAME: &str = "Computer Use";

/// Accessibility and Post Event share this pane. Apple maps both TCC services to it.
pub const ACCESSIBILITY_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";
/// Screen Recording.
pub const SCREEN_CAPTURE_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";
/// Input Monitoring (`ListenEvent`).
pub const LISTEN_EVENT_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent";

/// macOS permissions that must be granted for `permissions_ok`.
///
/// The macOS doctor is the only non-test caller. Other OS builds keep it for tests.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const MACOS_REQUIRED: &[&str] = &[
    "accessibility",
    "screen_recording",
    "post_events",
    "input_monitoring",
];

/// Flags for one `doctor` call. Both default to false so a normal call does not prompt or open Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DoctorOptions {
    /// Call the request APIs. The default only preflights.
    pub prompt: bool,
    /// Open the first missing Settings pane.
    pub open_settings: bool,
    /// Linux only: caller passed `--x11` to force the X11 detection.
    /// Other platforms ignore the field so the dispatcher can pass it
    /// unconditionally.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub x11: bool,
}

/// Values the platform measured. Assembly does not measure them again.
#[derive(Debug, Clone, Default)]
pub struct DoctorInput {
    /// Accessibility trust.
    pub accessibility: Option<bool>,
    /// Screen Recording preflight.
    pub screen_recording: Option<bool>,
    /// Post Event preflight.
    pub post_events: Option<bool>,
    /// Input Monitoring preflight. Not the event tap.
    pub input_monitoring: Option<bool>,
    /// P6 event tap in this process. Null when not attempted.
    pub input_monitor: Option<bool>,
    /// Running executable.
    pub path: Option<String>,
    /// Bundle id of the running binary.
    pub bundle_id: Option<String>,
    /// Running inside `Computer Use.app`.
    pub bundled: bool,
    /// `signed`, `unsigned`, `adhoc`, or `unknown`.
    pub signature: Option<String>,
    /// Apple team id.
    pub team_id: Option<String>,
    /// TCC responsible app.
    pub responsible: Option<ResponsibleApp>,
    /// Linux session kind.
    pub session: Option<String>,
    /// Linux AT-SPI bus address present.
    pub atspi_bus: Option<bool>,
    /// Linux XDG portal feature compiled in.
    pub portal: Option<bool>,
    /// Windows UIAccess.
    pub uiaccess: Option<bool>,
    /// Windows integrity label.
    pub integrity: Option<String>,
    /// Notes already collected by the platform.
    pub notes: Vec<String>,
    /// Permission ids that must be `Some(true)`.
    pub required: &'static [&'static str],
    /// Add the helper-bundle symlink note.
    pub recommend_bundle: bool,
    /// Caller passed `--open-settings` on a platform that can open a pane.
    pub open_settings: bool,
    /// `--prompt` was passed where it cannot prompt.
    pub prompt_ignored: bool,
    /// `--open-settings` was passed where it cannot open a pane.
    pub open_settings_ignored: bool,
}

/// Reads trust. `prompt` true means the platform may show a dialog.
///
/// The macOS doctor is the only non-test caller.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub trait PermissionProbe {
    /// Accessibility. `AXIsProcessTrusted` unless `prompt`.
    fn accessibility(&mut self, prompt: bool) -> Option<bool>;
    /// Screen Recording preflight, or the request API when `prompt`.
    fn screen_recording(&mut self, prompt: bool) -> Option<bool>;
    /// Post Event preflight, or the request API when `prompt`.
    fn post_events(&mut self, prompt: bool) -> Option<bool>;
    /// Input Monitoring preflight, or the request API when `prompt`.
    fn input_monitoring(&mut self, prompt: bool) -> Option<bool>;
}

/// Four probe results.
///
/// The macOS doctor is the only non-test caller.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeSample {
    /// Accessibility.
    pub accessibility: Option<bool>,
    /// Screen Recording.
    pub screen_recording: Option<bool>,
    /// Post Event.
    pub post_events: Option<bool>,
    /// Input Monitoring.
    pub input_monitoring: Option<bool>,
}

/// Call each probe once. `prompt` false must not ask the probe to show a dialog.
///
/// The macOS doctor is the only non-test caller.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn sample_permissions(probe: &mut impl PermissionProbe, prompt: bool) -> ProbeSample {
    ProbeSample {
        accessibility: probe.accessibility(prompt),
        screen_recording: probe.screen_recording(prompt),
        post_events: probe.post_events(prompt),
        input_monitoring: probe.input_monitoring(prompt),
    }
}

/// Build the report. Required probes that are not `Some(true)` are missing.
pub fn assemble(input: DoctorInput) -> DoctorReport {
    let target = enable_target(&input);
    let mut missing = Vec::new();
    let mut fix = Vec::new();
    for name in input.required {
        if probe_value(&input, name) != Some(true) {
            missing.push((*name).to_string());
            fix.push(permission_fix(name, &target));
        }
    }
    let fixes = fix.iter().map(|item| item.instruction.clone()).collect();
    let mut notes = input.notes.clone();
    if input.input_monitor == Some(false) {
        notes.push("user-input pause unavailable".to_string());
    }
    if input.recommend_bundle {
        notes.push(bundle_recommendation());
    }
    if input.prompt_ignored {
        notes.push("--prompt has no effect on this platform".to_string());
    }
    if input.open_settings_ignored {
        notes.push("--open-settings has no effect on this platform".to_string());
    } else if input.open_settings && fix.is_empty() {
        notes.push("no missing permission pane to open".to_string());
    }
    let note = join_notes(&notes);
    let identity = DoctorIdentity {
        path: input.path.clone(),
        bundle_id: input.bundle_id.clone(),
        bundled: input.bundled,
        signature: input.signature.clone(),
        team_id: input.team_id.clone(),
        responsible: input.responsible.clone(),
    };
    let mut report = DoctorReport {
        permissions_ok: missing.is_empty(),
        missing,
        accessibility: input.accessibility,
        screen_capture: input.screen_recording,
        screen_recording: input.screen_recording,
        post_events: input.post_events,
        input_monitoring: input.input_monitoring,
        input_monitor: input.input_monitor,
        bundle_id: input.bundle_id.clone(),
        identity,
        responsible_app: input.responsible.clone(),
        fix,
        fixes,
        session: input.session.clone(),
        atspi_bus: input.atspi_bus,
        portal: input.portal,
        uiaccess: input.uiaccess,
        integrity: input.integrity.clone(),
        text: String::new(),
        note,
    };
    report.text = render_text(&report);
    report
}

/// Settings URL of the first missing permission, when the caller asked to open it.
///
/// The macOS doctor is the only non-test caller.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn settings_url_to_open(report: &DoctorReport, open: bool) -> Option<&str> {
    if !open {
        return None;
    }
    report.fix.first().map(|item| item.url.as_str())
}

/// Append `text` to the note and to the readable summary.
///
/// The macOS doctor is the only caller.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn push_note(report: &mut DoctorReport, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    report.note = Some(match report.note.take() {
        Some(note) if !note.is_empty() => format!("{note}; {text}"),
        _ => text.to_string(),
    });
    if report.text.is_empty() {
        report.text = text.to_string();
    } else {
        report.text.push('\n');
        report.text.push_str(text);
    }
}

/// True when `executable` or `bundle_path` is `Computer Use.app`, or the bundle id matches.
///
/// The macOS doctor is the only non-test caller.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn is_helper_bundle(
    executable: Option<&str>,
    bundle_id: Option<&str>,
    bundle_path: Option<&str>,
) -> bool {
    if bundle_id == Some(HELPER_BUNDLE_ID) {
        return true;
    }
    let marker = format!("/{HELPER_APP_NAME}.app");
    let hit = |path: &str| path.contains(&marker);
    executable.is_some_and(hit) || bundle_path.is_some_and(hit)
}

/// Tell the user to symlink the bundled executable so TCC sticks to the bundle id.
pub fn bundle_recommendation() -> String {
    format!(
        "running binary is not inside {HELPER_APP_NAME}.app, so TCC will not attach to {HELPER_BUNDLE_ID}. Symlink the bundled executable into ~/.local/bin: ln -sfn \"/Applications/{HELPER_APP_NAME}.app/Contents/MacOS/computer-use\" \"$HOME/.local/bin/computer-use\""
    )
}

/// `x11`, `wayland`, or `x11+wayland`. Empty values count as unset.
///
/// The Linux doctor is the only caller. Other OS builds keep the function for tests.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn session_kind(display: Option<&str>, wayland: Option<&str>) -> Option<&'static str> {
    let display = nonempty(display);
    let wayland = nonempty(wayland);
    match (display, wayland) {
        (true, true) => Some("x11+wayland"),
        (true, false) => Some("x11"),
        (false, true) => Some("wayland"),
        (false, false) => None,
    }
}

/// True when `AT_SPI_BUS` is set or `runtime_dir/at-spi/bus*` exists.
///
/// The Linux doctor is the only caller. Other OS builds keep the function for tests.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn atspi_bus_present(at_spi_bus: Option<&str>, runtime_dir: Option<&std::path::Path>) -> bool {
    if nonempty(at_spi_bus) {
        return true;
    }
    let Some(runtime_dir) = runtime_dir else {
        return false;
    };
    let Ok(entries) = std::fs::read_dir(runtime_dir.join("at-spi")) else {
        return false;
    };
    entries
        .flatten()
        .any(|entry| entry.file_name().to_string_lossy().starts_with("bus"))
}

/// `codesign -dv` text. Does not run `codesign`.
///
/// The macOS doctor is the only non-test caller.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn parse_codesign(text: &str) -> (&'static str, Option<String>) {
    let lower = text.to_ascii_lowercase();
    if lower.contains("not signed at all") || lower.contains("code object is not signed") {
        return ("unsigned", None);
    }
    let team = team_id_from(text);
    let adhoc = text.contains("Signature=adhoc") || text.contains("(adhoc");
    if adhoc {
        return ("adhoc", team);
    }
    if team.is_some()
        || text.contains("Authority=")
        || text.contains("Signature size=")
        || text.contains("TeamIdentifier=")
    {
        return ("signed", team);
    }
    ("unknown", None)
}

/// Last RID of an integrity SID such as `S-1-16-8192`.
///
/// The Windows doctor parses this. Other OS builds keep it for tests.
///
/// P12a: still kept around because the shared `parse_token_lines`
/// unit test exercises it; the live Windows doctor no longer shells
/// out, but the helpers stay so the parsing contract has a
/// representative test. Always allow `dead_code` because call sites
/// are conditional.
#[allow(dead_code)]
pub fn integrity_from_sid(sid: &str) -> Option<&'static str> {
    let rid: u32 = sid.rsplit('-').next()?.trim().parse().ok()?;
    Some(match rid {
        0x5000.. => "protected",
        0x4000..=0x4FFF => "system",
        0x3000..=0x3FFF => "high",
        0x2000..=0x2FFF => "medium",
        0x1000..=0x1FFF => "low",
        _ => "untrusted",
    })
}

/// Mandatory label from `whoami /groups`.
///
/// The Windows doctor parses this. Other OS builds keep it for tests.
/// See the note on `integrity_from_sid`.
#[allow(dead_code)]
pub fn integrity_from_whoami(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    if lower.contains("protected process mandatory level") {
        return Some("protected");
    }
    if lower.contains("system mandatory level") {
        return Some("system");
    }
    if lower.contains("high mandatory level") {
        return Some("high");
    }
    if lower.contains("medium mandatory level") {
        return Some("medium");
    }
    if lower.contains("low mandatory level") {
        return Some("low");
    }
    if lower.contains("untrusted mandatory level") {
        return Some("untrusted");
    }
    None
}

/// `uiaccess=` / `integrity=` / `integrity_sid=` lines, or a whoami group listing.
///
/// Kept for the shared unit test in `doctor::tests`.
#[allow(dead_code)]
pub fn parse_token_lines(text: &str) -> (Option<bool>, Option<String>) {
    let mut uiaccess = None;
    let mut integrity = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("uiaccess=") {
            uiaccess = match value.trim() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            };
        } else if let Some(value) = line.strip_prefix("integrity=") {
            let value = value.trim();
            if !value.is_empty() {
                integrity = Some(value.to_string());
            }
        } else if let Some(value) = line.strip_prefix("integrity_sid=") {
            if let Some(label) = integrity_from_sid(value.trim()) {
                integrity = Some(label.to_string());
            }
        }
    }
    if integrity.is_none() {
        integrity = integrity_from_whoami(text).map(str::to_string);
    }
    (uiaccess, integrity)
}

/// Path of this executable, when the OS will say.
///
/// The path is resolved through symlinks. The install layout points
/// `~/.local/bin/computer-use` at the executable inside the app bundle, and
/// on macOS `current_exe` reports the path as it was invoked. Without the
/// resolution the binary inside the bundle would look like a bare binary,
/// and the recommendation the doctor gives would not satisfy its own check.
pub fn current_executable() -> Option<String> {
    let path = std::env::current_exe().ok()?;
    let resolved = std::fs::canonicalize(&path).unwrap_or(path);
    Some(resolved.to_string_lossy().into_owned())
}

/// Fix for one permission id. Unknown ids use the Privacy & Security root.
pub fn permission_fix(permission: &str, target: &str) -> PermissionFix {
    let (url, instruction) = match permission {
        "screen_recording" | "screen_capture" => (
            SCREEN_CAPTURE_SETTINGS_URL,
            format!("System Settings → Privacy & Security → Screen Recording → enable {target}"),
        ),
        "input_monitoring" => (
            LISTEN_EVENT_SETTINGS_URL,
            format!("System Settings → Privacy & Security → Input Monitoring → enable {target}"),
        ),
        "post_events" => (
            ACCESSIBILITY_SETTINGS_URL,
            format!(
                "System Settings → Privacy & Security → Accessibility → enable {target} (Post Event uses this pane)"
            ),
        ),
        "accessibility" => (
            ACCESSIBILITY_SETTINGS_URL,
            format!("System Settings → Privacy & Security → Accessibility → enable {target}"),
        ),
        _ => (
            "x-apple.systempreferences:com.apple.preference.security",
            format!("System Settings → Privacy & Security → enable {target}"),
        ),
    };
    PermissionFix {
        permission: permission.to_string(),
        url: url.to_string(),
        target: target.to_string(),
        instruction,
    }
}

fn probe_value(input: &DoctorInput, name: &str) -> Option<bool> {
    match name {
        "accessibility" => input.accessibility,
        "screen_recording" | "screen_capture" => input.screen_recording,
        "post_events" => input.post_events,
        "input_monitoring" => input.input_monitoring,
        _ => None,
    }
}

fn enable_target(input: &DoctorInput) -> String {
    if input.bundled {
        return HELPER_APP_NAME.to_string();
    }
    if let Some(app) = &input.responsible {
        if !app.name.is_empty() {
            return app.name.clone();
        }
    }
    if let Some(path) = &input.path {
        if !path.is_empty() {
            return path.clone();
        }
    }
    "computer-use".to_string()
}

fn join_notes(notes: &[String]) -> Option<String> {
    let text = notes
        .iter()
        .map(|note| note.trim())
        .filter(|note| !note.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn team_id_from(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("TeamIdentifier=") else {
            continue;
        };
        let rest = rest.trim();
        if rest.is_empty() || rest.eq_ignore_ascii_case("not set") {
            return None;
        }
        return Some(rest.to_string());
    }
    None
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn nonempty(value: Option<&str>) -> bool {
    value.is_some_and(|text| !text.trim().is_empty())
}

fn fmt_bool(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "null",
    }
}

fn render_text(report: &DoctorReport) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "permissions_ok: {}",
        if report.permissions_ok { "yes" } else { "no" }
    ));
    if report.missing.is_empty() {
        lines.push("missing: none".to_string());
    } else {
        lines.push(format!("missing: {}", report.missing.join(", ")));
    }
    lines.push(format!("accessibility: {}", fmt_bool(report.accessibility)));
    lines.push(format!(
        "screen_recording: {}",
        fmt_bool(report.screen_recording)
    ));
    lines.push(format!("post_events: {}", fmt_bool(report.post_events)));
    lines.push(format!(
        "input_monitoring: {}",
        fmt_bool(report.input_monitoring)
    ));
    lines.push(format!("input_monitor: {}", fmt_bool(report.input_monitor)));
    lines.push(format!(
        "binary: {}",
        report.identity.path.as_deref().unwrap_or("unknown")
    ));
    lines.push(format!(
        "bundle_id: {}",
        report.identity.bundle_id.as_deref().unwrap_or("null")
    ));
    lines.push(format!(
        "bundled: {}",
        if report.identity.bundled { "yes" } else { "no" }
    ));
    lines.push(format!(
        "signature: {}",
        report.identity.signature.as_deref().unwrap_or("null")
    ));
    if let Some(team) = &report.identity.team_id {
        lines.push(format!("team_id: {team}"));
    }
    if let Some(app) = &report.identity.responsible {
        let bundle = app.bundle_id.as_deref().unwrap_or("null");
        lines.push(format!("responsible: {} ({bundle})", app.name));
    }
    if let Some(session) = &report.session {
        lines.push(format!("session: {session}"));
    }
    if let Some(atspi) = report.atspi_bus {
        lines.push(format!(
            "atspi_bus: {}",
            if atspi { "present" } else { "absent" }
        ));
    }
    if let Some(portal) = report.portal {
        lines.push(format!(
            "portal: {}",
            if portal { "compiled" } else { "absent" }
        ));
    }
    if let Some(uiaccess) = report.uiaccess {
        lines.push(format!("uiaccess: {}", if uiaccess { "yes" } else { "no" }));
    }
    if let Some(integrity) = &report.integrity {
        lines.push(format!("integrity: {integrity}"));
    }
    for item in &report.fix {
        lines.push(item.instruction.clone());
        lines.push(item.url.clone());
    }
    if let Some(note) = &report.note {
        lines.push(note.clone());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct CountingProbe {
        value: Option<bool>,
        prompts: u32,
        reads: u32,
    }

    impl CountingProbe {
        fn count(&mut self, prompt: bool) {
            if prompt {
                self.prompts += 1;
            } else {
                self.reads += 1;
            }
        }
    }

    impl PermissionProbe for CountingProbe {
        fn accessibility(&mut self, prompt: bool) -> Option<bool> {
            self.count(prompt);
            self.value
        }
        fn screen_recording(&mut self, prompt: bool) -> Option<bool> {
            self.count(prompt);
            self.value
        }
        fn post_events(&mut self, prompt: bool) -> Option<bool> {
            self.count(prompt);
            self.value
        }
        fn input_monitoring(&mut self, prompt: bool) -> Option<bool> {
            self.count(prompt);
            self.value
        }
    }

    fn terminal() -> ResponsibleApp {
        ResponsibleApp {
            name: "Terminal".into(),
            bundle_id: Some("com.apple.Terminal".into()),
            pid: 20,
            path: Some("/System/Applications/Utilities/Terminal.app".into()),
        }
    }

    fn macos_input(screen: bool) -> DoctorInput {
        DoctorInput {
            accessibility: Some(true),
            screen_recording: Some(screen),
            post_events: Some(true),
            input_monitoring: Some(true),
            input_monitor: None,
            path: Some("/usr/local/bin/computer-use".into()),
            bundle_id: None,
            bundled: false,
            signature: Some("adhoc".into()),
            team_id: None,
            responsible: Some(terminal()),
            required: MACOS_REQUIRED,
            recommend_bundle: true,
            ..DoctorInput::default()
        }
    }

    #[test]
    fn default_sample_does_not_prompt() {
        let mut probe = CountingProbe {
            value: Some(false),
            prompts: 0,
            reads: 0,
        };
        let sample = sample_permissions(&mut probe, false);
        assert_eq!(probe.prompts, 0);
        assert_eq!(probe.reads, 4);
        assert_eq!(sample.accessibility, Some(false));
        assert_eq!(sample.screen_recording, Some(false));
        assert_eq!(sample.post_events, Some(false));
        assert_eq!(sample.input_monitoring, Some(false));
    }

    #[test]
    fn prompt_flag_asks_every_probe() {
        let mut probe = CountingProbe {
            value: Some(true),
            prompts: 0,
            reads: 0,
        };
        let sample = sample_permissions(&mut probe, true);
        assert_eq!(probe.prompts, 4);
        assert_eq!(probe.reads, 0);
        assert_eq!(sample.post_events, Some(true));
    }

    #[test]
    fn missing_permissions_and_fixes() {
        let report = assemble(macos_input(false));
        assert!(!report.permissions_ok);
        assert_eq!(report.missing, vec!["screen_recording".to_string()]);
        assert_eq!(report.screen_capture, Some(false));
        assert_eq!(report.screen_recording, Some(false));
        assert!(report.input_monitor.is_none());
        assert_eq!(report.fix.len(), 1);
        assert_eq!(report.fix[0].permission, "screen_recording");
        assert_eq!(report.fix[0].url, SCREEN_CAPTURE_SETTINGS_URL);
        assert_eq!(report.fix[0].target, "Terminal");
        assert_eq!(report.fixes[0], report.fix[0].instruction);
        assert!(report.fixes[0].contains("Screen Recording"));
        assert!(report.fixes[0].contains("enable Terminal"));
        assert_eq!(
            report.responsible_app.as_ref().map(|app| app.name.as_str()),
            Some("Terminal")
        );
        assert!(report.note.as_deref().unwrap().contains(HELPER_BUNDLE_ID));
        assert!(report.text.contains("permissions_ok: no"));
        assert!(report.text.contains("missing: screen_recording"));
        assert!(settings_url_to_open(&report, false).is_none());
        assert_eq!(
            settings_url_to_open(&report, true),
            Some(SCREEN_CAPTURE_SETTINGS_URL)
        );
    }

    #[test]
    fn granted_report_has_nothing_to_open() {
        let mut input = macos_input(true);
        input.recommend_bundle = false;
        input.bundled = true;
        input.bundle_id = Some(HELPER_BUNDLE_ID.into());
        input.open_settings = true;
        let report = assemble(input);
        assert!(report.permissions_ok);
        assert!(report.missing.is_empty());
        assert!(report.fix.is_empty());
        assert!(report.fixes.is_empty());
        assert!(settings_url_to_open(&report, true).is_none());
        assert!(report
            .note
            .as_deref()
            .unwrap()
            .contains("no missing permission pane to open"));
        assert!(report.text.contains("permissions_ok: yes"));
        assert!(report.text.contains("no missing permission pane to open"));
    }

    #[test]
    fn denied_probes_map_every_pane() {
        let report = assemble(DoctorInput {
            accessibility: Some(false),
            screen_recording: Some(false),
            post_events: Some(false),
            input_monitoring: Some(false),
            input_monitor: Some(false),
            bundled: true,
            bundle_id: Some(HELPER_BUNDLE_ID.into()),
            required: MACOS_REQUIRED,
            ..DoctorInput::default()
        });
        assert_eq!(
            report.missing,
            vec![
                "accessibility".to_string(),
                "screen_recording".to_string(),
                "post_events".to_string(),
                "input_monitoring".to_string(),
            ]
        );
        assert_eq!(report.fix[0].url, ACCESSIBILITY_SETTINGS_URL);
        assert_eq!(report.fix[1].url, SCREEN_CAPTURE_SETTINGS_URL);
        assert_eq!(report.fix[2].url, ACCESSIBILITY_SETTINGS_URL);
        assert!(report.fix[2].instruction.contains("Post Event"));
        assert_eq!(report.fix[3].url, LISTEN_EVENT_SETTINGS_URL);
        assert_eq!(report.fix[0].target, HELPER_APP_NAME);
        assert!(report
            .note
            .as_deref()
            .unwrap()
            .contains("user-input pause unavailable"));
        assert!(!report.permissions_ok);
    }

    #[test]
    fn json_shape_matches_the_snapshot() {
        let report = assemble(DoctorInput {
            accessibility: Some(false),
            screen_recording: Some(true),
            post_events: Some(true),
            input_monitoring: Some(true),
            input_monitor: None,
            path: Some("/tmp/computer-use".into()),
            bundle_id: None,
            bundled: false,
            signature: Some("unsigned".into()),
            responsible: Some(terminal()),
            required: MACOS_REQUIRED,
            ..DoctorInput::default()
        });
        let mut value = serde_json::to_value(&report).unwrap();
        let text = value.as_object_mut().unwrap().remove("text").unwrap();
        assert!(text.as_str().unwrap().contains("permissions_ok: no"));
        assert!(text.as_str().unwrap().contains("bundle_id: null"));
        assert_eq!(
            value,
            json!({
                "permissions_ok": false,
                "missing": ["accessibility"],
                "accessibility": false,
                "screen_capture": true,
                "screen_recording": true,
                "post_events": true,
                "input_monitoring": true,
                "input_monitor": null,
                "bundle_id": null,
                "identity": {
                    "path": "/tmp/computer-use",
                    "bundle_id": null,
                    "bundled": false,
                    "signature": "unsigned",
                    "team_id": null,
                    "responsible": {
                        "name": "Terminal",
                        "bundle_id": "com.apple.Terminal",
                        "pid": 20,
                        "path": "/System/Applications/Utilities/Terminal.app"
                    }
                },
                "responsible_app": {
                    "name": "Terminal",
                    "bundle_id": "com.apple.Terminal",
                    "pid": 20,
                    "path": "/System/Applications/Utilities/Terminal.app"
                },
                "fix": [{
                    "permission": "accessibility",
                    "url": ACCESSIBILITY_SETTINGS_URL,
                    "target": "Terminal",
                    "instruction": "System Settings → Privacy & Security → Accessibility → enable Terminal"
                }],
                "fixes": ["System Settings → Privacy & Security → Accessibility → enable Terminal"],
                "session": null,
                "atspi_bus": null,
                "portal": null,
                "uiaccess": null,
                "integrity": null
            })
        );
    }

    #[test]
    fn empty_required_list_stays_ok() {
        let report = assemble(DoctorInput {
            notes: vec!["accessibility and screen-capture probes land in P13".into()],
            session: Some("wayland".into()),
            atspi_bus: Some(false),
            ..DoctorInput::default()
        });
        assert!(report.permissions_ok);
        assert!(report.missing.is_empty());
        assert!(report.accessibility.is_none());
        assert_eq!(report.session.as_deref(), Some("wayland"));
        assert_eq!(report.atspi_bus, Some(false));
        assert!(report.note.unwrap().contains("P13"));
    }

    #[test]
    fn codesign_parser() {
        let (status, team) = parse_codesign("target: code object is not signed at all\n");
        assert_eq!(status, "unsigned");
        assert!(team.is_none());

        let (status, team) =
            parse_codesign("Signature=adhoc\nflags=0x2(adhoc)\nTeamIdentifier=not set\n");
        assert_eq!(status, "adhoc");
        assert!(team.is_none());

        let (status, team) = parse_codesign(
            "Authority=Developer ID Application: Example (ABCDE12345)\nTeamIdentifier=ABCDE12345\nSignature size=9000\n",
        );
        assert_eq!(status, "signed");
        assert_eq!(team.as_deref(), Some("ABCDE12345"));

        let (status, team) = parse_codesign("codesign: No such file or directory\n");
        assert_eq!(status, "unknown");
        assert!(team.is_none());
    }

    #[test]
    fn session_and_bundle_helpers() {
        assert_eq!(session_kind(Some(":0"), None), Some("x11"));
        assert_eq!(session_kind(None, Some("wayland-0")), Some("wayland"));
        assert_eq!(
            session_kind(Some(":0"), Some("wayland-0")),
            Some("x11+wayland")
        );
        assert_eq!(session_kind(Some("  "), Some("")), None);
        assert!(is_helper_bundle(None, Some(HELPER_BUNDLE_ID), None));
        assert!(is_helper_bundle(
            Some("/tmp/Computer Use.app/Contents/MacOS/computer-use"),
            None,
            None
        ));
        assert!(!is_helper_bundle(Some("/tmp/computer-use"), None, None));
    }

    #[test]
    fn a_symlinked_helper_is_still_the_helper() {
        // The install layout points the install directory at the executable
        // inside the bundle. Resolving the link has to keep the bundle
        // recognizable, or the doctor recommends a layout that fails its own
        // check.
        let dir = std::env::temp_dir().join(format!(
            "computer-use-bundle-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let bundle = dir.join("Computer Use.app/Contents/MacOS");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&bundle).expect("the bundle directory is creatable");
        let real = bundle.join("computer-use");
        std::fs::write(&real, b"#!/bin/sh\n").expect("the helper is writable");
        let link = dir.join("computer-use");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).expect("the symlink is creatable");
        #[cfg(windows)]
        std::fs::copy(&real, &link).expect("the copy is writable");

        let resolved = std::fs::canonicalize(&link).unwrap_or(link.clone());
        assert!(
            is_helper_bundle(Some(&resolved.to_string_lossy()), None, None),
            "{resolved:?} should be recognized as the helper bundle"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn atspi_presence_and_token_lines() {
        let dir = std::env::temp_dir().join(format!(
            "computer-use-atspi-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!atspi_bus_present(None, Some(&dir)));
        assert!(atspi_bus_present(Some("unix:path=/tmp/bus"), Some(&dir)));
        std::fs::create_dir_all(dir.join("at-spi")).unwrap();
        std::fs::write(dir.join("at-spi").join("bus"), b"unix:abstract=/at-spi").unwrap();
        assert!(atspi_bus_present(None, Some(&dir)));
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(
            parse_token_lines("uiaccess=false\nintegrity_sid=S-1-16-8192\n"),
            (Some(false), Some("medium".into()))
        );
        assert_eq!(
            parse_token_lines("uiaccess=true\nintegrity=high\n"),
            (Some(true), Some("high".into()))
        );
        assert_eq!(
            parse_token_lines("Mandatory Label\\Medium Mandatory Level\n"),
            (None, Some("medium".into()))
        );
        assert_eq!(integrity_from_sid("S-1-16-12288"), Some("high"));
        assert_eq!(integrity_from_sid("S-1-16-16384"), Some("system"));
        assert_eq!(integrity_from_sid("not-a-sid"), None);
    }

    #[test]
    fn open_settings_without_a_fix_returns_none() {
        let report = DoctorReport::blank();
        assert!(settings_url_to_open(&report, true).is_none());
        assert!(settings_url_to_open(&report, false).is_none());
    }
}

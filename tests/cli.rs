//! Binary tests that do not touch the desktop.
//!
//! Every test runs against its own `XDG_*` tree, so two tests running in
//! parallel cannot see each other's cache, policy, or pause marker, and a test
//! cannot write `~/.cache/computer-use`. The tree is removed when the test's
//! thread ends, including while unwinding from a failed assertion.

use std::cell::RefCell;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_computer-use")
}

/// A temp directory that removes itself on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "computer-use-{label}-{}-{nanos}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn child(&self, name: &str) -> String {
        let dir = self.0.join(name);
        std::fs::create_dir_all(&dir).expect("temp child dir");
        dir.to_string_lossy().into_owned()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

thread_local! {
    /// One tree per test thread. `libtest` runs each test on its own thread, so
    /// this is per-test isolation without a `tempfile` dependency, and the
    /// `Drop` runs on panic too.
    static TEST_DIR: RefCell<Option<TempDir>> = const { RefCell::new(None) };
}

fn with_test_dir<T>(f: impl FnOnce(&TempDir) -> T) -> T {
    TEST_DIR.with(|slot| {
        let mut slot = slot.borrow_mut();
        let dir = slot.get_or_insert_with(|| TempDir::new("cli"));
        f(dir)
    })
}

fn cache_home() -> String {
    with_test_dir(|dir| dir.child("cache"))
}

fn config_home() -> String {
    with_test_dir(|dir| dir.child("config"))
}

fn state_home() -> String {
    with_test_dir(|dir| dir.child("state"))
}

/// A path that does not exist, so the admin policy file is absent.
fn missing_admin() -> String {
    with_test_dir(|dir| {
        dir.path()
            .join("no-admin-policy.toml")
            .to_string_lossy()
            .into_owned()
    })
}

fn command(args: &[&str]) -> Command {
    let mut command = Command::new(bin());
    command
        .args(args)
        .env("XDG_CACHE_HOME", cache_home())
        .env("XDG_CONFIG_HOME", config_home())
        .env("XDG_STATE_HOME", state_home())
        .env("COMPUTER_USE_ADMIN_POLICY", missing_admin())
        .env_remove("COMPUTER_USE_CONFIRM_CMD");
    command
}

fn run(args: &[&str]) -> std::process::Output {
    command(args).output().expect("spawn computer-use")
}

#[test]
fn help_lists_commands() {
    let output = run(&["--help"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    for needle in [
        "computer-use",
        "screenshot",
        "click",
        "move",
        "type",
        "key",
        "scroll",
        "get-state",
        "wait",
        "displays",
        "list-apps",
        "list-windows",
        "launch",
        "activate",
        "quit",
        "doctor",
        "tree",
        "session",
        "serve",
        "exec",
        "press",
        "set-value",
        "select-text",
        "perform-action",
        "scroll-to",
        "focus",
        "drag",
        "mouse-down",
        "mouse-up",
        "hold-key",
        "clipboard",
        "zoom",
        "triple-click",
        "--pretty",
        "--json",
        "--app",
        "--window",
        "--element",
        "--require-visible",
        "--full",
        "--session",
        "--state",
        "--screenshot",
        "--coord",
        "--adapter",
        "--tier",
        "--synthetic-focus",
        "--allow-foreground",
        "--require-background",
        "--no-overlay",
        "--display",
        "isolation",
        "overlay",
    ] {
        assert!(stdout.contains(needle), "help missing {needle}:\n{stdout}");
    }
}

#[test]
fn screenshot_help_mentions_canvas() {
    let output = run(&["screenshot", "--help"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--canvas"),
        "screenshot help missing --canvas:\n{stdout}"
    );
}

#[test]
fn element_commands_reject_bad_arguments_without_acting() {
    let missing = run(&["press", "--count", "1"]);
    let missing_value: Value = serde_json::from_slice(&missing.stdout).unwrap();
    assert_eq!(missing_value["ok"], false);
    assert_eq!(missing_value["error"]["code"], "invalid_arguments");
    assert!(missing_value["error"]["message"]
        .as_str()
        .unwrap()
        .contains("element"));

    let count = run(&["press", "--element", "1", "--count", "4"]);
    let count_value: Value = serde_json::from_slice(&count.stdout).unwrap();
    assert_eq!(count_value["error"]["code"], "invalid_arguments");

    let deleted = run(&["perform-action", "--element", "1", "--name", "AXDelete"]);
    let deleted_value: Value = serde_json::from_slice(&deleted.stdout).unwrap();
    assert_eq!(deleted_value["error"]["code"], "invalid_arguments");
    assert!(deleted_value["error"]["message"]
        .as_str()
        .unwrap()
        .contains("AXDelete"));

    let range = run(&[
        "select-text",
        "--element",
        "1",
        "--start",
        "5",
        "--end",
        "1",
    ]);
    let range_value: Value = serde_json::from_slice(&range.stdout).unwrap();
    assert_eq!(range_value["error"]["code"], "invalid_arguments");
    assert!(range_value["error"]["message"]
        .as_str()
        .unwrap()
        .contains("start"));

    let both = run(&["click", "--element", "1", "--x", "2", "--y", "3"]);
    let both_value: Value = serde_json::from_slice(&both.stdout).unwrap();
    assert_eq!(both_value["error"]["code"], "invalid_arguments");

    let alias = run(&["scroll-to-visible", "--help"]);
    assert!(alias.status.success());
    let help = String::from_utf8_lossy(&alias.stdout);
    assert!(
        help.contains("scroll") || help.contains("element"),
        "{help}"
    );

    let exec = run(&[
        "exec",
        r#"{"action":"perform_action","element":1,"name":"AXDelete"}"#,
    ]);
    let exec_value: Value = serde_json::from_slice(&exec.stdout).unwrap();
    assert_eq!(exec_value["error"]["code"], "invalid_arguments");

    let bare = run(&["exec", r#"{"action":"click"}"#]);
    let bare_value: Value = serde_json::from_slice(&bare.stdout).unwrap();
    assert_eq!(bare_value["error"]["code"], "invalid_arguments");

    let hyphen = run(&[
        "exec",
        r#"{"action":"select-text","element":1,"start":2,"end":1}"#,
    ]);
    let hyphen_value: Value = serde_json::from_slice(&hyphen.stdout).unwrap();
    assert_eq!(hyphen_value["error"]["code"], "invalid_arguments");
}

#[test]
fn element_commands_are_unsupported_off_macos_and_windows() {
    // Element actions are real on macOS (Accessibility), Windows (UIA,
    // since P12b), and Linux (AT-SPI, since P13a). No platform returns
    // `unsupported` anymore; the contract is that the call fails with
    // some structured error (most commonly `app_not_found` or
    // `execution_failed` on the Linux runner without an at-spi bus).
    if cfg!(target_os = "macos") || cfg!(target_os = "windows") || cfg!(target_os = "linux") {
        return;
    }
    let output = run(&["focus", "--element", "0"]);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout was not JSON: {} stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "unsupported");
    let message = value["error"]["message"].as_str().unwrap();
    assert!(message.contains(std::env::consts::OS), "{message}");
}

#[test]
fn tree_command_is_unsupported_off_macos_and_windows() {
    // `tree` is real on macOS (P12a), Windows (UIA), and Linux (AT-SPI,
    // P13a). Other platforms — none today — would still expect the
    // `unsupported` envelope. The skip list reflects exactly that.
    if cfg!(target_os = "macos") || cfg!(target_os = "windows") || cfg!(target_os = "linux") {
        return;
    }
    let output = run(&["tree"]);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout was not JSON: {} stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "unsupported");
    let message = value["error"]["message"].as_str().unwrap();
    assert!(message.contains(std::env::consts::OS), "{message}");
}

#[test]
fn p9_commands_are_unsupported_off_macos() {
    // macOS-only IPC surface (menus, Apple events, dock/spaces,
    // shortcuts). macOS itself and Windows (P12a) keep these
    // unsupported; Linux also stays unsupported.
    if cfg!(target_os = "macos") || cfg!(target_os = "windows") {
        return;
    }
    let cases: &[&[&str]] = &[
        &["menu", "list", "--app", "Finder"],
        &["menu", "click", "--app", "Finder", "--path", "File > Save"],
        &["apple-event", "--app", "Finder", "--script", "get name"],
        &["shortcuts", "run", "Open URL"],
        &["dock", "list"],
        &["dock", "click", "--title", "Finder"],
        &["spaces"],
    ];
    for args in cases {
        let output = run(args);
        let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
            panic!(
                "{:?} stdout was not JSON: {} stderr: {}",
                args,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        assert_eq!(
            value["ok"], false,
            "{:?} expected ok=false: {}",
            args, value
        );
        assert_eq!(
            value["error"]["code"], "unsupported",
            "{:?} expected unsupported: {}",
            args, value
        );
        let message = value["error"]["message"].as_str().unwrap();
        assert!(
            message.contains(std::env::consts::OS),
            "{:?} message missing os: {message}",
            args
        );
    }
}

#[test]
fn p9_help_lists_new_commands() {
    let output = run(&["--help"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for needle in ["menu", "apple-event", "shortcuts", "dock", "spaces"] {
        assert!(stdout.contains(needle), "help missing {needle}:\n{stdout}");
    }
}

#[test]
fn p9_exec_round_trip_for_each_action() {
    let actions: &[&str] = &[
        r#"{"action":"menu_list","app":"Notes","depth":4}"#,
        r#"{"action":"menu_click","app":"Notes","path":"File > Save"}"#,
        r#"{"action":"apple_event","app":"Notes","script":"get name"}"#,
        r#"{"action":"apple_event","app":"Notes","script":"get name","language":"javascript"}"#,
        r#"{"action":"shortcuts_run","name":"Open URL"}"#,
        r#"{"action":"dock_list"}"#,
        r#"{"action":"dock_click","title":"Finder"}"#,
        r#"{"action":"spaces"}"#,
    ];
    for action in actions {
        let output = run(&["exec", action]);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        // The action is either ok=true (binary present / stub happy) or
        // ok=false with a documented error code. Anything else, especially
        // an empty body, is a regression.
        assert!(
            !stdout.trim().is_empty(),
            "exec {action} produced no JSON: stderr: {stderr}"
        );
        let value: Value = serde_json::from_str(&stdout).unwrap_or_else(|_| {
            panic!("exec {action} stdout was not JSON: {stdout} stderr: {stderr}")
        });
        assert!(value["ok"].is_boolean(), "{value}");
    }
}

#[test]
fn version_prints_package_version() {
    let output = run(&["--version"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("computer-use"));
    // Read the version from the manifest so a release bump cannot leave this
    // test asserting a stale number.
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("Cargo.toml is readable");
    let version = manifest
        .lines()
        .find_map(|line| line.strip_prefix("version = \""))
        .and_then(|rest| rest.split('"').next())
        .expect("Cargo.toml declares a version");
    assert!(stdout.contains(version), "{stdout} should carry {version}");
}

#[test]
fn wait_prints_one_json_object() {
    let output = run(&["wait", "--ms", "1"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert_eq!(value["ok"], true);
    assert_eq!(value["waited_ms"], 1);
    assert!(value.get("error").is_none());
}

#[test]
fn pretty_wait_is_still_one_json_value() {
    let output = run(&["--pretty", "wait", "--ms", "0"]);
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains('\n'));
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["ok"], true);
    assert_eq!(value["waited_ms"], 0);
}

#[test]
fn wait_without_duration_is_a_json_error() {
    let output = run(&["wait"]);
    assert!(!output.status.success());
    let value: Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "invalid_arguments");
}

/// Every flag-parsing branch reports one JSON object with a stable code.
#[test]
fn cli_flag_matrix_reports_clean_errors() {
    let cases: &[(&[&str], Option<&str>)] = &[
        (
            &["--tier", "bogus", "wait", "--ms", "0"],
            Some("invalid_arguments"),
        ),
        (
            &["--coord", "bogus", "wait", "--ms", "0"],
            Some("invalid_arguments"),
        ),
        (
            &["--adapter", "bogus", "wait", "--ms", "0"],
            Some("invalid_arguments"),
        ),
        (
            &["--display", "999", "screenshot", "--output", "/tmp/x.png"],
            Some("invalid_arguments"),
        ),
        (
            &[
                "--policy",
                "/definitely/missing.toml",
                "tree",
                "--app",
                "Finder",
            ],
            Some("invalid_arguments"),
        ),
        (
            &["--cdp", "--agent-overlay", "bogus", "browser", "sessions"],
            Some("invalid_arguments"),
        ),
        (&["exec", "not json"], Some("invalid_arguments")),
        (
            &["exec", r#"{"action":"definitely_not_an_action"}"#],
            Some("invalid_arguments"),
        ),
        (
            &["exec", r#"{"action":"wait","ms":1,"timeout":5000}"#],
            Some("invalid_arguments"),
        ),
        // Read-only actions ignore the post-state flags instead of failing.
        (&["--state", "wait", "--ms", "0"], None),
        (&["--state-diff", "wait", "--ms", "0"], None),
        (&["--screenshot", "wait", "--ms", "0"], None),
    ];
    for (args, code) in cases {
        let output = run(args);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("panicked at"),
            "{}: {stderr}",
            args.join(" ")
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let value: Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|error| panic!("{}: {stdout:?} ({error})", args.join(" ")));
        assert!(value.get("ok").is_some(), "{}: {value}", args.join(" "));
        match code {
            Some(code) => assert_eq!(value["error"]["code"], *code, "{}: {value}", args.join(" ")),
            None => assert_eq!(value["ok"], true, "{}: {value}", args.join(" ")),
        }
    }
}

/// The CDP gate reports the build state: `unsupported` without the feature,
/// `cdp_unavailable` when the feature is on but no endpoint answers.
#[test]
fn cli_cdp_gate_reports_the_feature_state() {
    let output = run(&["--cdp", "--cdp-url", "ws://127.0.0.1:1", "browser", "tabs"]);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let expected = if cfg!(feature = "cdp") {
        "cdp_unavailable"
    } else {
        "unsupported"
    };
    assert_eq!(value["error"]["code"], expected, "{value}");
}

/// A malformed policy file is reported before any action runs.
#[test]
fn cli_bad_policy_file_is_a_clean_error() {
    let dir = TempDir::new("bad-policy");
    let policy = dir.path().join("policy.toml");
    std::fs::write(&policy, "version = 1\ndefault = \"sometimes\"\n").unwrap();
    let output = run(&[
        "--policy",
        policy.to_str().unwrap(),
        "tree",
        "--app",
        "Finder",
    ]);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["code"], "invalid_arguments", "{value}");
    assert!(value["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("line "));
}

#[test]
fn serve_wait_roundtrip() {
    assert_json_wait(&["serve"]);
}

#[test]
fn json_flag_wait_roundtrip() {
    assert_json_wait(&["--json"]);
}

/// Run the binary with an isolated state directory.
fn run_in_state(args: &[&str], state: &str) -> std::process::Output {
    let mut command = Command::new(bin());
    command
        .args(args)
        .env("XDG_CACHE_HOME", cache_home())
        .env("XDG_CONFIG_HOME", config_home())
        .env("XDG_STATE_HOME", state)
        .env("COMPUTER_USE_ADMIN_POLICY", missing_admin())
        .env_remove("COMPUTER_USE_CONFIRM_CMD");
    command.output().expect("spawn computer-use")
}

/// Exercise every top-level command once.
///
/// The state directory starts paused, so mutating actions answer `paused`
/// before any input is posted or any app is touched. Read-only commands run
/// for real (a capture, a list, a permission probe). Every call must print one
/// JSON object and must not panic.
#[test]
fn cli_surface_sweep_is_guarded_and_well_formed() {
    let dir = TempDir::new("sweep");
    let state = dir.child("state");
    let shot = dir.path().join("shot.png").to_string_lossy().into_owned();

    let paused = run_in_state(&["pause"], &state);
    assert!(
        paused.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&paused.stderr)
    );

    let invocations: &[&[&str]] = &[
        // Read-only surface.
        &["displays"],
        &["isolation"],
        &["cursor-position"],
        &["get-state"],
        &["doctor"],
        &["doctor", "--full"],
        &["list-apps"],
        &["list-apps", "--all", "--match", "N*"],
        &["list-windows"],
        &["list-windows", "--all"],
        &["policy", "check", "--app", "Finder"],
        &["session", "prune", "--dry-run"],
        &["audit", "path"],
        &["audit", "tail", "--lines", "5"],
        &["spaces"],
        &["spaces", "move", "--to-space", "2"],
        &["window", "info"],
        &["window", "state", "--app", "no-such-app-xyz"],
        &["screenshot", "--region", "0,0,2,2", "--output", &shot],
        &["screenshot", "--app", "Finder", "--output", &shot],
        &[
            "screenshot",
            "--app",
            "Finder",
            "--marks",
            "--output",
            &shot,
        ],
        &["zoom", "--region", "0,0,2,2", "--output", &shot],
        &[
            "zoom", "--app", "Finder", "--region", "0,0,2,2", "--output", &shot,
        ],
        &["session", "end"],
        &["tree", "--app", "Finder"],
        &["ocr", "--app", "Finder"],
        &["ocr", "--image", "/definitely/missing/image.png"],
        &["wait", "--ms", "1"],
        &["wait", "--text", "x", "--timeout-ms", "50"],
        &["verify", "--element", "1", "--element-state", "bogus"],
        &["clipboard", "get"],
        // Mutating surface: the pause marker refuses each one.
        &["click", "--x", "1", "--y", "1"],
        &["click", "--x", "1", "--y", "1", "--count", "2"],
        &["move", "--x", "1", "--y", "1"],
        &["type", "--text", "sweep"],
        &["key", "cmd+s"],
        &["hold-key", "--keys", "a", "--ms", "1"],
        &["scroll", "--dy", "1"],
        &["drag", "--path", "1,1,2,2"],
        &["mouse-down", "--x", "1", "--y", "1"],
        &["mouse-up", "--x", "1", "--y", "1"],
        &["press", "--app", "Finder", "--element", "0"],
        &["focus", "--app", "Finder", "--element", "0"],
        &[
            "set-value",
            "--app",
            "Finder",
            "--element",
            "0",
            "--value",
            "x",
        ],
        &["select-text", "--app", "Finder", "--element", "0"],
        &[
            "perform-action",
            "--app",
            "Finder",
            "--element",
            "0",
            "--name",
            "AXPress",
        ],
        &["scroll-to", "--app", "Finder", "--element", "0"],
        &["clipboard", "set", "--text", "sweep"],
        &["launch", "--app", "no-such-app-xyz"],
        &["activate", "--app", "no-such-app-xyz"],
        &["quit", "--app", "no-such-app-xyz"],
        &["window", "move", "--x", "1", "--y", "1"],
        &["window", "resize", "--width", "100", "--height", "100"],
        &["window", "set", "--width", "100"],
        // The rest of the surface, refused before acting.
        &["menu", "list", "--app", "Finder"],
        &["menu", "click", "--app", "Finder", "--path", "File > Save"],
        &["apple-event", "--app", "Finder", "--script", "get name"],
        &["shortcuts", "run", "no-such-shortcut"],
        &["dock", "list"],
        &["dock", "click", "--title", "no-such-dock-item"],
        &["overlay"],
        &["browser"],
        &["--cdp", "browser", "sessions"],
        &["exec", r#"{"action":"isolation"}"#],
        &["batch", r#"{"action":"batch","actions":[]}"#],
    ];

    for args in invocations {
        let output = run_in_state(args, &state);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("panicked at"),
            "`{}` panicked: {stderr}",
            args.join(" ")
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let value: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
            panic!(
                "`{}` did not print one JSON object: {stdout:?} ({error})",
                args.join(" ")
            )
        });
        assert!(
            value.get("ok").is_some(),
            "`{}` returned no ok field: {value}",
            args.join(" ")
        );
    }

    let resumed = run_in_state(&["resume"], &state);
    assert!(
        resumed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    // `dir` drops here and removes the whole tree, on panic too.
}

/// Every browser action is decodable through `exec` even when the CDP
/// feature is built in but no browser is reachable.
#[test]
fn exec_sweeps_the_browser_actions() {
    let actions: &[&str] = &[
        "browser_tabs",
        "browser_activate",
        "browser_navigate",
        "browser_back",
        "browser_forward",
        "browser_reload",
        "browser_wait",
        "browser_snapshot",
        "browser_click",
        "browser_text",
        "browser_type",
        "browser_press",
        "browser_hover",
        "browser_scroll",
        "browser_select",
        "browser_check",
        "browser_upload",
        "browser_dialog",
        "browser_screenshot",
        "browser_eval",
        "browser_tab_new",
        "browser_tab_close",
        "browser_launch",
        "browser_quit",
        "browser_sessions",
        "browser_batch",
    ];
    for action in actions {
        let line = format!(r#"{{"action":"{action}","target_id":"none"}}"#);
        let output = run(&["exec", &line]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("panicked at"),
            "{action} panicked: {stderr}"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let value: Value = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|error| panic!("{action}: {stdout:?} ({error})"));
        assert!(value.get("ok").is_some(), "{action}: {value}");
    }
}

fn assert_json_wait(args: &[&str]) {
    let mut child = command(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        stdin
            .write_all(b"{\"id\":1,\"action\":\"wait\",\"ms\":1}\n")
            .unwrap();
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().next().expect("response line");
    let value: Value = serde_json::from_str(line).unwrap();
    assert_eq!(value["ok"], true);
    assert_eq!(value["id"], 1);
    assert_eq!(value["waited_ms"], 1);
}

#[test]
fn policy_check_reports_builtin_and_user_deny() {
    let output = run(&["policy", "check", "--app", "com.apple.Terminal"]);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout: {} stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(value["ok"], true, "{value}");
    assert_eq!(value["decision"], "deny");
    assert_eq!(value["source"], "builtin");
    assert_eq!(value["bundle_id"], "com.apple.Terminal");

    let dir = std::env::temp_dir().join(format!("computer-use-cli-policy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let policy = dir.join("policy.toml");
    std::fs::write(
        &policy,
        "version = 1\n[[deny]]\nbundle_id = \"com.example.Blocked\"\n",
    )
    .unwrap();
    let output = command(&[
        "--policy",
        policy.to_str().expect("utf-8"),
        "policy",
        "check",
        "--app",
        "com.example.Blocked",
    ])
    .output()
    .unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["decision"], "deny", "{value}");
    assert_eq!(value["source"], "user");

    let audit = std::path::Path::new(&state_home())
        .join("computer-use")
        .join("audit.jsonl");
    let text = std::fs::read_to_string(&audit).unwrap_or_default();
    assert!(text.contains("\"action\":\"policy_check\""), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn doctor_returns_json_on_every_platform() {
    let output = run(&["doctor"]);
    assert!(
        output.status.success(),
        "stderr: {} stdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let value: Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert_eq!(value["ok"], true);
    assert!(value["permissions_ok"].is_boolean());
    assert!(value["missing"].is_array());
    assert!(value["accessibility"].is_boolean() || value["accessibility"].is_null());
    assert!(value["screen_capture"].is_boolean() || value["screen_capture"].is_null());
    assert!(value["screen_recording"].is_boolean() || value["screen_recording"].is_null());
    assert!(value["post_events"].is_boolean() || value["post_events"].is_null());
    assert!(value["input_monitoring"].is_boolean() || value["input_monitoring"].is_null());
    assert!(value["input_monitor"].is_null());
    assert!(value["fix"].is_array());
    assert!(value["fixes"].is_array());
    assert!(value["text"].is_string());
    assert!(value["identity"].is_object());
    assert!(value["bundle_id"].is_string() || value["bundle_id"].is_null());
    #[cfg(target_os = "macos")]
    if value["accessibility"] == false {
        assert_eq!(value["permissions_ok"], false);
        let missing = value["missing"].as_array().unwrap();
        assert!(missing.iter().any(|item| item == "accessibility"));
    }
}

#[test]
fn doctor_help_lists_prompt_flags() {
    let output = run(&["doctor", "--help"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--prompt"), "{stdout}");
    assert!(stdout.contains("--request"), "{stdout}");
    assert!(stdout.contains("--open-settings"), "{stdout}");
    assert!(stdout.contains("exits 0"), "{stdout}");
}

#[test]
fn session_prune_dry_run_stays_in_the_temp_cache() {
    let output = run(&["session", "prune", "--dry-run"]);
    assert!(
        output.status.success(),
        "stderr: {} stdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let value: Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert_eq!(value["ok"], true);
    assert_eq!(value["action"], "session_prune");
    assert_eq!(value["dry_run"], true);
    assert_eq!(value["ttl_secs"], 86400);
    assert!(value["removed"].as_array().unwrap().is_empty());
    assert!(
        !cache_home().contains("/.cache/computer-use"),
        "test cache escaped: {}",
        cache_home()
    );
}

#[test]
fn launch_terminal_is_denied_on_every_os() {
    let output = run(&["launch", "com.apple.Terminal"]);
    assert!(!output.status.success());
    let value: Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "policy_denied", "{value}");
    let message = value["error"]["message"].as_str().unwrap();
    assert!(message.contains("com.apple.Terminal"), "{message}");
}

#[test]
fn list_apps_returns_json() {
    let output = run(&["list-apps"]);
    let value: Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout))
        .expect("list-apps stdout is JSON");
    if cfg!(target_os = "linux") {
        // Linux backend still returns `unsupported` for the macOS-style
        // listing. macOS and Windows (P12a) populate the apps list
        // honestly via the platform-native enumerator.
        assert!(!output.status.success());
        assert_eq!(value["error"]["code"], "unsupported");
    } else {
        assert!(output.status.success(), "{value}");
        assert!(value["apps"].is_array());
    }
}

#[test]
fn activate_without_app_is_a_json_error() {
    let output = run(&["activate"]);
    assert_eq!(output.status.code(), Some(1));
    let value: Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "invalid_arguments");
}

#[test]
fn exec_wait_roundtrip() {
    let output = run(&[r#"exec"#, r#"{"id":9,"action":"wait","ms":1}"#]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert_eq!(value["ok"], true);
    assert_eq!(value["id"], 9);
    assert_eq!(value["waited_ms"], 1);
}

#[test]
fn exec_wait_element_condition_reaches_the_action() {
    // An invalid element state proves the field was handed to the wait
    // action rather than swallowed by the shared request context.
    let output = run(&[
        "exec",
        r#"{"action":"wait","element":12,"element_state":"bogus"}"#,
    ]);
    let value: Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "invalid_arguments");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("element state"),
        "{value:?}"
    );

    let verify = run(&[
        "exec",
        r#"{"action":"verify","element":3,"element_state":"bogus"}"#,
    ]);
    let value: Value = serde_json::from_str(&String::from_utf8_lossy(&verify.stdout)).unwrap();
    assert_eq!(value["error"]["code"], "invalid_arguments");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("element state"),
        "{value:?}"
    );
}

#[test]
fn exec_wait_rejects_a_duration_with_a_condition() {
    let output = run(&["exec", r#"{"action":"wait","ms":5,"text":"x"}"#]);
    let value: Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert_eq!(value["error"]["code"], "invalid_arguments");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("duration or a condition"),
        "{value:?}"
    );
}

#[test]
fn help_lists_browser_command() {
    let output = run(&["--help"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("browser"),
        "missing browser in help: {stdout}"
    );
}

#[test]
fn browser_subcommand_without_cdp_flag_is_invalid_arguments() {
    let output = run(&["browser", "tabs"]);
    assert_eq!(output.status.code(), Some(1));
    let value: Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout))
        .expect("browser tabs response is JSON");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "invalid_arguments");
}

#[test]
fn browser_subcommand_help_lists_subs() {
    let output = run(&["browser", "--help"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("tabs"));
    assert!(stdout.contains("activate"));
    assert!(stdout.contains("navigate"));
    assert!(stdout.contains("click"));
    assert!(stdout.contains("text"));
    assert!(stdout.contains("type"));
    assert!(stdout.contains("eval"));
}

#[test]
fn browser_navigate_in_exec_without_cdp_returns_unsupported() {
    // The CLI --cdp flag is off in this invocation, so the JSON request fails
    // with `unsupported` before it ever opens a websocket.
    let body = r#"{"action":"browser_navigate","url":"https://example.com"}"#;
    let output = run(&["exec", body]);
    let value: Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "unsupported");
}

#[test]
#[cfg(feature = "cdp")]
fn browser_text_in_exec_without_cdp_returns_unsupported() {
    // `browser_text` now reads innerText; class Observe. Without the --cdp
    // flag, dispatch refuses with `unsupported` regardless of payload.
    let body = r#"{"action":"browser_text","selector":"h1"}"#;
    let output = run(&["exec", body]);
    let value: Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "unsupported");
}

#[test]
#[cfg(feature = "cdp")]
fn browser_type_in_exec_without_cdp_returns_unsupported() {
    let body = r##"{"action":"browser_type","text":"hi","selector":"#name"}"##;
    let output = run(&["exec", body]);
    let value: Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "unsupported");
}

#[test]
#[cfg(feature = "cdp")]
fn browser_navigate_blocks_url_before_connecting_with_temp_policy() {
    // Spin up a temp policy.toml blocking https://blocked.example.com/*. With
    // --cdp pointing at an unreachable port, navigate must return
    // blocked_url without ever trying to connect. We pass `--policy` because
    // tests do not read the ambient XDG_CONFIG_HOME.
    let policy_dir =
        std::env::temp_dir().join(format!("computer-use-cli-p10-block-{}", std::process::id()));
    std::fs::create_dir_all(&policy_dir).expect("policy dir");
    let policy_path = policy_dir.join("policy.toml");
    std::fs::write(
        &policy_path,
        "version = 1\n\n[browser]\nblocked_urls = [\"https://blocked.example.com/*\"]\n",
    )
    .expect("policy file");

    let policy_path_string = policy_path.to_string_lossy().to_string();
    let output = Command::new(bin())
        .args([
            "--cdp",
            "--cdp-url",
            "ws://127.0.0.1:1",
            "--policy",
            &policy_path_string,
            "browser",
            "navigate",
            "--url",
            "https://blocked.example.com/page",
        ])
        .env("XDG_CACHE_HOME", cache_home())
        .env("XDG_CONFIG_HOME", config_home())
        .env("XDG_STATE_HOME", state_home())
        .env("COMPUTER_USE_ADMIN_POLICY", missing_admin())
        .env_remove("COMPUTER_USE_CONFIRM_CMD")
        .output()
        .expect("binary run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_str(&stdout).expect("json response");
    assert_eq!(value["ok"], false, "stderr: {stdout}");
    assert_eq!(
        value["error"]["code"], "blocked_url",
        "expected blocked_url before the connect attempt; got {stdout}"
    );
    let _ = std::fs::remove_file(&policy_path);
}

#[test]
#[cfg(feature = "cdp")]
fn browser_tabs_against_missing_chrome_returns_cdp_unavailable() {
    // With --cdp-url pointing at an unreachable websocket, attach must
    // fail with cdp_unavailable (transport error), not new_page. There is
    // no fallback path. We do not use --app here because list_apps is
    // macOS-only and would fail before we ever reach the CDP path on
    // Linux / Windows runners.
    let output = Command::new(bin())
        .args(["--cdp", "--cdp-url", "ws://127.0.0.1:1", "browser", "tabs"])
        .env("XDG_CACHE_HOME", cache_home())
        .env("XDG_CONFIG_HOME", config_home())
        .env("XDG_STATE_HOME", state_home())
        .env("COMPUTER_USE_ADMIN_POLICY", missing_admin())
        .env_remove("COMPUTER_USE_CONFIRM_CMD")
        .output()
        .expect("binary run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_str(&stdout).expect("json response");
    assert_eq!(value["ok"], false, "stderr: {stdout}");
    assert_eq!(
        value["error"]["code"], "cdp_unavailable",
        "expected cdp_unavailable (no chrome / no page target); got {stdout}"
    );
}

// ---------------------------------------------------------------------------
// P14: isolation and the `--display` filter.
//
// None of these touch the desktop: `isolation` is pure, and `--display` is
// refused with `invalid_arguments` before any capture happens.
// ---------------------------------------------------------------------------

#[test]
fn isolation_prints_the_matrix_on_every_platform() {
    let output = run(&["--pretty", "isolation"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_str(&stdout).expect("json response");
    assert_eq!(value["ok"], true, "stdout: {stdout}");

    let platform = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "windows"
    };
    assert_eq!(value["platform"], platform);
    assert_eq!(value["display_filter"], true);
    assert_eq!(value["xvfb"], "linux only");
    assert_eq!(value["vm"], "documented, not shipped");
    assert_eq!(
        value["xvfb_recipe"]["server_command"],
        "Xvfb :99 -screen 0 1440x900x24"
    );
    assert_eq!(value["xvfb_recipe"]["starts_server"], false);

    // The infeasible list is the documentation's, in JSON form.
    for key in [
        "macos_vm_per_agent",
        "share_host_app_into_guest",
        "hide_virtual_display_from_user",
        "windows_virtual_display",
        "nested_wayland_input_without_portal",
    ] {
        let reason = value["infeasible"][key]
            .as_str()
            .unwrap_or_else(|| panic!("infeasible.{key} missing from {stdout}"));
        assert!(!reason.is_empty(), "{key} needs a reason");
    }
}

#[test]
fn a_bad_display_value_is_refused_before_the_backend_is_touched() {
    // Syntax is pure, so this fails the same way on a runner with no display
    // server at all — which is exactly what ubuntu-latest is.
    for spec in ["left", "-1", "1.5", ""] {
        let output = run(&["--display", spec, "move", "--x", "10", "--y", "10"]);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let value: Value =
            serde_json::from_str(&stdout).unwrap_or_else(|_| panic!("{spec:?} → {stdout}"));
        assert_eq!(value["ok"], false, "{spec:?} stdout: {stdout}");
        assert_eq!(
            value["error"]["code"], "invalid_arguments",
            "{spec:?} stdout: {stdout}"
        );
    }
}

#[test]
fn an_unknown_display_id_never_captures() {
    // Whether the id can be resolved depends on the machine: a CI runner has no
    // enumerable display, so the lookup itself fails first. What must hold on
    // every platform is that the action stops there and never captures, and that
    // the resolution itself is `invalid_arguments` — which the stub test in
    // `protocol::tests` proves exactly, with a fake display list.
    let output = run(&["--display", "99999", "screenshot"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_str(&stdout).expect("json response");
    assert_eq!(value["ok"], false, "stdout: {stdout}");
    let code = value["error"]["code"].as_str().expect("code");
    let message = value["error"]["message"].as_str().expect("message");
    if code == "invalid_arguments" {
        assert!(
            message.contains("display 99999 was not found"),
            "stdout: {stdout}"
        );
    } else {
        // The platform could not enumerate displays at all.
        assert_eq!(code, "execution_failed", "stdout: {stdout}");
    }
    // No screenshot was written, so there is no map and no path in the body.
    assert!(value.get("path").is_none(), "stdout: {stdout}");
}

#[test]
fn display_virtual_needs_an_explicit_exec_and_never_starts_a_server() {
    let output = run(&["--pretty", "--display", "virtual"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_str(&stdout).expect("json response");

    if cfg!(target_os = "linux") {
        assert_eq!(value["ok"], true, "stdout: {stdout}");
        assert_eq!(value["server_command"], "Xvfb :99 -screen 0 1440x900x24");
        assert_eq!(value["display"], ":99");
        assert_eq!(value["starts_server"], false);
    } else {
        assert_eq!(value["ok"], false, "stdout: {stdout}");
        assert_eq!(
            value["error"]["code"], "unsupported",
            "only Linux has a supported recipe; stdout: {stdout}"
        );
    }
}

#[test]
fn exec_without_virtual_is_an_argument_error() {
    let output = run(&["--exec", "true"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_str(&stdout).expect("json response");
    assert_eq!(value["ok"], false, "stdout: {stdout}");
    assert_eq!(
        value["error"]["code"], "invalid_arguments",
        "stdout: {stdout}"
    );
    assert_eq!(output.status.code(), Some(2));
}

#[cfg(target_os = "linux")]
#[test]
fn display_virtual_exec_runs_inside_the_prepared_environment() {
    // `env` prints its environment, so the exec really happened with DISPLAY set
    // and the recipe's server is not needed for this assertion.
    let output = run(&["--display", "virtual", "--exec", "--", "env"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.lines().any(|line| line == "DISPLAY=:99"),
        "DISPLAY was not set for the exec'd command: {stdout}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn display_virtual_exec_reports_a_command_it_cannot_execute() {
    // A real command name that is not on PATH: exec fails, and the error is
    // structured rather than a panic or a silent success.
    let output = run(&[
        "--display",
        "virtual",
        "--exec",
        "--",
        "computer-use-no-such-binary",
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_str(&stdout).expect("json response");
    assert_eq!(value["ok"], false, "stdout: {stdout}");
    assert_eq!(
        value["error"]["code"], "execution_failed",
        "stdout: {stdout}"
    );
    assert_eq!(output.status.code(), Some(1));
}

#[cfg(target_os = "linux")]
#[test]
fn display_virtual_exec_without_a_command_is_an_argument_error() {
    // clap refuses `--exec` with no value at parse time.
    let output = run(&["--display", "virtual", "--exec"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_str(&stdout).expect("json response");
    assert_eq!(value["ok"], false, "stdout: {stdout}");
    assert_eq!(
        value["error"]["code"], "invalid_arguments",
        "stdout: {stdout}"
    );
}

/// Scroll parses bare negative values (`--dy -3`) the same as `--dy=-3`.
/// The target app does not exist, so dispatch stops at app resolution and
/// no real wheel event is posted on the test host.
fn assert_scroll_parses(args: &[&str]) {
    let mut full = vec!["scroll"];
    full.extend_from_slice(args);
    full.extend_from_slice(&["--app", "computer-use-no-such-app-xyz"]);
    let output = run(&full);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], false, "{value}");
    assert_ne!(
        value["error"]["code"], "invalid_arguments",
        "scroll {args:?} must parse: {value}"
    );
}

#[test]
fn scroll_accepts_negative_deltas_without_equal_sign() {
    assert_scroll_parses(&["--dy=-3"]);
    assert_scroll_parses(&["--dy", "-3"]);
}

#[test]
fn scroll_accepts_negative_dx_and_dy_in_one_invocation() {
    assert_scroll_parses(&["--dx", "-1", "--dy", "-3"]);
    assert_scroll_parses(&["--x", "-10", "--y", "5", "--dy", "2"]);
}

#[test]
fn scroll_negative_amount_with_background_tier_parses() {
    assert_scroll_parses(&[
        "--amount",
        "-3",
        "--direction",
        "up",
        "--tier",
        "event",
        "--require-background",
    ]);
}

/// Modes that answer before a desktop action: `--json` with a subcommand,
/// no arguments, a non-native adapter, and the synthetic tier.
#[test]
fn special_modes_report_clean_errors_or_run() {
    let output = run(&["--json", "displays"]);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["code"], "invalid_arguments", "{value}");

    let output = run(&[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Usage"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );

    let output = run(&["--adapter", "openai", "displays"]);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["code"], "invalid_arguments", "{value}");
    assert!(value["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("exec"));

    // The synthetic tier is only consulted for actions that plan input; a
    // sleep-only wait still succeeds.
    let output = run(&["--synthetic-focus", "wait", "--ms", "0"]);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], true, "{value}");
}

#[test]
fn browser_batch_rejects_bad_json_and_shapes() {
    for (args, needle) in [
        (vec!["--cdp", "browser", "batch", "not json"], "needs JSON"),
        (
            vec!["--cdp", "browser", "batch", r#"{"nope":1}"#],
            "needs a JSON array",
        ),
        (
            vec!["--cdp", "browser", "batch", r#"[{"action":"bogus"}]"#],
            "actions are invalid",
        ),
    ] {
        let output = run(&args);
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["error"]["code"], "invalid_arguments", "{value}");
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains(needle),
            "{needle}: {value}"
        );
    }
}

#[test]
fn serve_skips_blank_lines_and_reports_invalid_utf8() {
    let mut child = command(&["serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"\n\n")
        .expect("write");
    let output = child.wait_with_output().expect("serve output");
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty(), "{:?}", output.stdout);

    let mut child = command(&["serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(&[0xff, b'\n'])
        .expect("write");
    let output = child.wait_with_output().expect("serve output");
    assert_eq!(output.status.code(), Some(1));
    let value: Value = serde_json::from_slice(&output.stdout).expect("one JSON response");
    assert_eq!(value["error"]["code"], "io_error", "{value}");
}

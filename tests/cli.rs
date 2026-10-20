//! Binary tests that do not touch the desktop.
//!
//! Every command gets a temp `XDG_CACHE_HOME` so a test cannot write
//! `~/.cache/computer-use`.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use serde_json::Value;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_computer-use")
}

fn cache_home() -> &'static str {
    static DIR: OnceLock<String> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir =
            std::env::temp_dir().join(format!("computer-use-cli-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp cache dir");
        dir.to_string_lossy().into_owned()
    })
    .as_str()
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

fn config_home() -> &'static str {
    static DIR: OnceLock<String> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir =
            std::env::temp_dir().join(format!("computer-use-cli-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp config dir");
        dir.to_string_lossy().into_owned()
    })
    .as_str()
}

fn state_home() -> &'static str {
    static DIR: OnceLock<String> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir =
            std::env::temp_dir().join(format!("computer-use-cli-state-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp state dir");
        dir.to_string_lossy().into_owned()
    })
    .as_str()
}

fn missing_admin() -> &'static str {
    static PATH: OnceLock<String> = OnceLock::new();
    PATH.get_or_init(|| {
        std::env::temp_dir()
            .join(format!("computer-use-cli-no-admin-{}", std::process::id()))
            .to_string_lossy()
            .into_owned()
    })
    .as_str()
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
fn element_commands_are_unsupported_off_macos() {
    if cfg!(target_os = "macos") {
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
fn tree_command_is_unsupported_off_macos() {
    if cfg!(target_os = "macos") {
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
    if cfg!(target_os = "macos") {
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
    assert!(stdout.contains("0.1.0"), "{stdout}");
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

#[test]
fn serve_wait_roundtrip() {
    assert_json_wait(&["serve"]);
}

#[test]
fn json_flag_wait_roundtrip() {
    assert_json_wait(&["--json"]);
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

    let audit = std::path::Path::new(state_home())
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
    if cfg!(target_os = "macos") {
        assert!(output.status.success(), "{value}");
        assert!(value["apps"].is_array());
    } else {
        assert!(!output.status.success());
        assert_eq!(value["error"]["code"], "unsupported");
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

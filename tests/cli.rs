//! Binary tests that do not touch the desktop.

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::Value;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_computer-use")
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(bin())
        .args(args)
        .output()
        .expect("spawn computer-use")
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
        "--pretty",
        "--json",
        "--app",
        "--window",
        "--element",
        "--require-visible",
        "--full",
        "--session",
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
    let mut child = Command::new(bin())
        .args(args)
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
    assert!(value["accessibility"].is_boolean() || value["accessibility"].is_null());
    assert!(value["screen_capture"].is_boolean() || value["screen_capture"].is_null());
    assert!(value["input_monitor"].is_null());
    assert!(value["fixes"].is_array());
}

#[test]
fn launch_terminal_is_denied_or_unsupported() {
    let output = run(&["launch", "com.apple.Terminal"]);
    assert!(!output.status.success());
    let value: Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert_eq!(value["ok"], false);
    let code = value["error"]["code"].as_str().unwrap();
    if cfg!(target_os = "macos") {
        assert_eq!(code, "policy_denied", "{value}");
    } else {
        assert_eq!(code, "unsupported", "{value}");
        let message = value["error"]["message"].as_str().unwrap();
        assert!(message.contains(std::env::consts::OS), "{message}");
    }
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

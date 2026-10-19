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
        "serve",
        "exec",
        "--pretty",
        "--json",
    ] {
        assert!(stdout.contains(needle), "help missing {needle}:\n{stdout}");
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

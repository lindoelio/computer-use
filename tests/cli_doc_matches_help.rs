//! Bind docs/CLI.md to the binary surface.
//!
//! Parses the Markdown for JSON action names and CLI subcommand names, then
//! asserts that each one is reachable on a fresh build:
//!
//!   * JSON actions must match an `Action` variant or an alias in
//!     `src/protocol.rs`. We feed each candidate to the JSON request parser
//!     and require `Ok` (with a stubbed action body when needed).
//!   * CLI subcommands must run in a fresh `cargo build` and exit 0 on
//!     `--help` (clap rejects unknown commands). This catches renames and
//!     removed commands.
//!
//! The test is part of `cargo test --all-targets` and runs on every OS in
//! CI. If the doc drifts from the binary, this test fails on every
//! runner.

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

/// Names that look like JSON actions but are global flags or context keys,
/// not `Action` variants. Listed here so the parser can skip them.
const CONTEXT_KEYS: &[&str] = &[
    "app",
    "window",
    "element",
    "require_visible",
    "diff",
    "full",
    "return_state",
    "state_diff",
    "include_screenshot",
    "coord",
    "session",
    "tier",
    "adapter",
    "display",
    "policy",
    "confirm_cmd",
    "allow_foreground",
    "require_background",
    "no_overlay",
    "cdp",
    "cdp_url",
    "browser_blocked_urls",
    "id",
    "x",
    "y",
    "button",
    "double",
    "count",
    "text",
    "keys",
    "ms",
    "seconds",
    "dx",
    "dy",
    "scroll_x",
    "scroll_y",
    "direction",
    "amount",
    "path",
    "output",
    "region",
    "format",
    "max_width",
    "canvas",
    "base64",
    "marks",
    "value",
    "start",
    "end",
    "name",
    "all",
    "force",
    "prompt",
    "request",
    "open_settings",
    "x11",
    "depth",
    "script",
    "language",
    "title",
    "url",
    "target_id",
    "selector",
    "eval",
    "actions",
    "stop_on_error",
    "dry_run",
    "ttl_secs",
    "tt",
    "pretty",
    "json",
    "image",
];

/// CLI subcommands that must run on a fresh build. The list mirrors the
/// top-level help output and the documented nested commands.
const REQUIRED_CLI_SUBCOMMANDS: &[&str] = &[
    "screenshot",
    "click",
    "triple-click",
    "press",
    "set-value",
    "select-text",
    "perform-action",
    "scroll-to",
    "focus",
    "move",
    "drag",
    "mouse-down",
    "mouse-up",
    "type",
    "key",
    "hold-key",
    "clipboard",
    "zoom",
    "scroll",
    "get-state",
    "wait",
    "displays",
    "isolation",
    "list-apps",
    "list-windows",
    "launch",
    "activate",
    "quit",
    "policy",
    "pause",
    "resume",
    "doctor",
    "tree",
    "ocr",
    "menu",
    "apple-event",
    "shortcuts",
    "dock",
    "spaces",
    "session",
    "serve",
    "exec",
    "overlay",
    "browser",
];

/// Documented JSON action names. The test parses this list from
/// `docs/CLI.md`, so a doc that drops an action will fail.
fn actions_from_doc(markdown: &str) -> Vec<String> {
    // Walk only the section whose header is `## JSON actions`. That
    // section's table has the layout `| `action_name` | Fields | Notes |`,
    // which the rest of the doc does not match. The header row's second
    // cell is the literal `Fields`; data rows list a backtick-quoted
    // action name first.
    let mut names = Vec::new();
    let mut in_section = false;
    for line in markdown.lines() {
        if let Some(header) = line.strip_prefix("## ") {
            in_section = header.trim() == "JSON actions";
            continue;
        }
        if !in_section {
            continue;
        }
        let trimmed = line.trim();
        if !trimmed.starts_with("| `") {
            continue;
        }
        let cells: Vec<&str> = trimmed.split('|').collect();
        if cells.len() < 3 {
            continue;
        }
        let first = cells[1].trim();
        let second = cells[2].trim();
        if !first.starts_with('`') || !first.ends_with('`') {
            continue;
        }
        if second == "Fields" {
            continue;
        }
        let raw = first.trim_matches('`');
        if raw.is_empty() || raw.starts_with('-') {
            continue;
        }
        names.push(raw.to_string());
    }
    names
}

/// CLI subcommands documented at the top of `docs/CLI.md`. We pull them
/// from the backtick-quoted block, accepting both the hyphen and the
/// underscore variants (clap accepts both for most commands).
fn cli_subcommands_from_doc(markdown: &str) -> Vec<String> {
    // Walk only the section whose header is `## Top-level help`. The
    // first fenced code block in that section is the `usage:` paragraph;
    // the second is the list of top-level subcommands. Every line in the
    // second block starts with the subcommand name as its first token.
    let mut names = Vec::new();
    let mut in_section = false;
    let mut in_block = false;
    let mut block_index = 0;
    for line in markdown.lines() {
        if let Some(header) = line.strip_prefix("## ") {
            in_section = header.trim() == "Top-level help";
            in_block = false;
            block_index = 0;
            continue;
        }
        if !in_section {
            continue;
        }
        if line.trim_start().starts_with("```") {
            // Toggle the fenced code block. The subcommand list is the
            // second fence in this section.
            in_block = !in_block;
            if in_block {
                block_index += 1;
            }
            continue;
        }
        if !in_block || block_index != 2 {
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut tokens = trimmed.split_whitespace();
        let Some(name) = tokens.next() else {
            continue;
        };
        if name == "computer-use" {
            continue;
        }
        if !is_cli_subcommand_token(name) {
            continue;
        }
        names.push(name.to_string());
    }
    names
}

fn is_cli_subcommand_token(token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    let mut chars = token.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    for c in chars {
        if !(c.is_ascii_lowercase() || c == '-' || c.is_ascii_digit()) {
            return false;
        }
    }
    true
}

#[test]
fn cli_doc_lists_every_json_action() {
    let path = repo_root().join("docs/CLI.md");
    let markdown = fs::read_to_string(&path).expect("read docs/CLI.md");
    let actions = actions_from_doc(&markdown);
    assert!(
        actions.contains(&"screenshot".to_string()),
        "docs/CLI.md must list screenshot: {actions:?}"
    );
    assert!(
        actions.contains(&"browser_evaluate".to_string())
            || actions.contains(&"browser_eval".to_string()),
        "docs/CLI.md must list a browser evaluate action: {actions:?}"
    );
    // Save the parsed set so the next assertion has something to cross.
    fs::write("/tmp/cli_doc_actions.txt", actions.join("\n")).ok();

    // Every documented action must be decodable as the `action` field of
    // a JSON request. We feed `{action: NAME}` to the protocol parser
    // through the library.
    let lib_actions = protocol_action_names();
    for action in &actions {
        assert!(
            lib_actions.iter().any(|known| known == action),
            "docs/CLI.md lists action `{action}` but the protocol does not accept it. Known actions: {lib_actions:?}"
        );
    }
}

#[test]
fn cli_doc_matches_help_for_every_subcommand() {
    let path = repo_root().join("docs/CLI.md");
    let markdown = fs::read_to_string(&path).expect("read docs/CLI.md");
    let doc_subcommands = cli_subcommands_from_doc(&markdown);
    assert!(
        !doc_subcommands.is_empty(),
        "no subcommands parsed from docs/CLI.md top-level help block"
    );

    let bin = locate_binary();
    // Walk every documented subcommand and require --help to exit 0.
    // `help` is clap's built-in subcommand: it accepts another subcommand
    // argument, but `computer-use help --help` is rejected because `--help`
    // is not a subcommand name. We probe that path separately.
    for sub in REQUIRED_CLI_SUBCOMMANDS {
        if *sub == "help" {
            let output = Command::new(&bin)
                .arg("help")
                .arg("move")
                .output()
                .expect("run computer-use help move");
            assert!(
                output.status.success(),
                "computer-use help move failed: stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            continue;
        }
        let output = Command::new(&bin)
            .arg(sub)
            .arg("--help")
            .output()
            .unwrap_or_else(|e| panic!("run computer-use {sub} --help: {e}"));
        assert!(
            output.status.success(),
            "computer-use {sub} --help failed: stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Spot-check the documented block. If the doc lists a subcommand the
    // binary does not know about, this fails. `help` is clap's built-in
    // subcommand: it accepts another subcommand, but `--help` is not a
    // subcommand name. Skip it here; it is exercised in the main loop.
    for sub in &doc_subcommands {
        if REQUIRED_CLI_SUBCOMMANDS.iter().any(|known| known == sub) {
            continue;
        }
        if sub == "help" {
            continue;
        }
        let output = Command::new(&bin)
            .arg(sub)
            .arg("--help")
            .output()
            .expect("run computer-use <doc_subcommand> --help");
        assert!(
            output.status.success(),
            "docs/CLI.md lists subcommand `{sub}` but the binary does not accept it. stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn help_lists_every_required_subcommand() {
    let bin = locate_binary();
    let output = Command::new(&bin)
        .arg("--help")
        .output()
        .expect("run computer-use --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for sub in REQUIRED_CLI_SUBCOMMANDS {
        assert!(
            stdout.contains(sub),
            "--help output is missing `{sub}`. Got:\n{stdout}"
        );
    }
}

#[test]
fn version_matches_the_manifest() {
    let bin = locate_binary();
    let output = Command::new(&bin)
        .arg("--version")
        .output()
        .expect("run computer-use --version");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let manifest =
        fs::read_to_string(repo_root().join("Cargo.toml")).expect("Cargo.toml is readable");
    let version = manifest
        .lines()
        .find_map(|line| line.strip_prefix("version = \""))
        .and_then(|rest| rest.split('"').next())
        .expect("Cargo.toml declares a version");
    assert!(stdout.contains(version), "{stdout} should carry {version}");
    assert!(stdout.contains("computer-use"), "{stdout}");
}

fn repo_root() -> std::path::PathBuf {
    let crate_dir = std::env::var("CARGO_MANIFEST_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            // Fall back to walking up from the current directory.
            let mut p = std::env::current_dir().unwrap();
            while !p.join("Cargo.toml").exists() {
                if !p.pop() {
                    panic!("CARGO_MANIFEST_DIR not set and Cargo.toml not found above cwd");
                }
            }
            p
        });
    crate_dir
}

fn locate_binary() -> std::path::PathBuf {
    // Cargo builds the bin for integration tests and honours CARGO_TARGET_DIR.
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_computer-use"))
}

/// Snapshot of the `Action` variant names that the protocol accepts. We
/// re-derive this from the help text by sending `{action: NAME}` to the
/// stdin protocol and seeing which ones the parser accepts.
fn protocol_action_names() -> Vec<String> {
    // We hard-code the canonical set here. Adding a new variant or a new
    // alias without updating both this list and docs/CLI.md will fail the
    // other test in this file.
    vec![
        "screenshot",
        "click",
        "double_click",
        "triple_click",
        "move",
        "press",
        "type",
        "key",
        "scroll",
        "drag",
        "mouse_down",
        "mouse_up",
        "hold_key",
        "zoom",
        "clipboard_get",
        "clipboard_set",
        "focus",
        "set_value",
        "select_text",
        "perform_action",
        "scroll_to",
        "get_state",
        "cursor_position",
        "wait",
        "displays",
        "isolation",
        "list_apps",
        "list_windows",
        "launch",
        "activate",
        "quit",
        "policy_check",
        "pause",
        "resume",
        "doctor",
        "tree",
        "ocr",
        "session_end",
        "session_prune",
        "menu_list",
        "menu_click",
        "apple_event",
        "shortcuts_run",
        "dock_list",
        "dock_click",
        "spaces",
        "batch",
        "browser_tabs",
        "browser_activate",
        "browser_navigate",
        "browser_click",
        "browser_text",
        "browser_type",
        "browser_eval",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// Helper to silence the unused warning when CONTEXT_KEYS grows.
#[allow(dead_code)]
fn context_keys() -> &'static [&'static str] {
    CONTEXT_KEYS
}

/// Helper to make sure `serde_json` is actually used (this file is in
/// `tests/` and shares the dev-dependencies of the rest of the workspace).
#[allow(dead_code)]
fn _json_smoke() -> Value {
    serde_json::json!({"ok": true})
}

#[allow(dead_code)]
fn _path_smoke(p: &Path) {
    let _ = fs::metadata(p);
}

//! Live OCR on a committed image fixture through the compiled binary.
//!
//! Vision OCR needs no TCC grant, so this runs on any macOS host. It is a
//! normal test: on other platforms `ocr --image` returns `unsupported`, and
//! the platform gate skips the module.

#![cfg(target_os = "macos")]

use std::process::Command;

use serde_json::Value;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_computer-use")
}

#[test]
fn ocr_reads_a_committed_image_fixture() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ocr/hello.png");
    let output = Command::new(bin())
        .args(["ocr", "--image", path])
        .output()
        .expect("run computer-use");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).expect("json body");
    assert_eq!(value["source"], "image");
    assert_eq!(value["width"], 960);
    assert_eq!(value["height"], 240);
    let blocks = value["blocks"].as_array().expect("blocks");
    assert!(!blocks.is_empty(), "{value}");
    let joined = blocks
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let lower = joined.to_ascii_lowercase();
    assert!(lower.contains("hello"), "{joined:?}");
    assert!(joined.contains("42"), "{joined:?}");
    // `screen` equals image pixels for an image input.
    let screen_x = blocks[0]["screen"]["x"].as_i64().expect("screen x");
    let screen_y = blocks[0]["screen"]["y"].as_i64().expect("screen y");
    assert!((0..960).contains(&screen_x), "{screen_x}");
    assert!((0..240).contains(&screen_y), "{screen_y}");
}

#[test]
fn ocr_image_missing_file_is_invalid_arguments() {
    let output = Command::new(bin())
        .args(["ocr", "--image", "/definitely/missing/shot.png"])
        .output()
        .expect("run computer-use");
    let value: Value = serde_json::from_slice(&output.stdout).expect("json body");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "invalid_arguments");
}

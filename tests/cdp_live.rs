//! Live CDP tests against a real Chromium.
//!
//! The suite starts one headless browser for the file and one in-process HTTP
//! server, then runs the compiled binary against them. It is skipped when no
//! Chromium executable can be found, so `cargo test` stays green on a machine
//! without a browser. CI sets `CHROME_BIN` (or installs Chrome) and sets
//! `COMPUTER_USE_REQUIRE_LIVE=1`, which turns the skip into a failure: that job
//! is supposed to exercise a real browser.

#![cfg(feature = "cdp")]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_computer-use")
}

const INDEX: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Fixture App</title></head>
<body>
  <h1 id="title">Fixture App</h1>
  <button id="hello" onclick="document.getElementById('out').textContent='clicked'">Say hello</button>
  <p id="out">idle</p>
  <label for="name">Name</label>
  <input id="name" type="text" />
  <label for="country">Country</label>
  <select id="country"><option value="">choose</option><option value="pt">Portugal</option><option value="br">Brazil</option></select>
  <label><input id="agree" type="checkbox" /> Agree</label>
  <label for="password">Password</label>
  <input id="password" type="password" />
  <label for="file">File</label>
  <input id="file" type="file" />
  <button id="alert" onclick="alert('hi')">Alert</button>
  <div id="scrollbox" style="height:60px;overflow:auto;border:1px solid #000"><div style="height:300px">tall content</div></div>
  <a id="next" href="/next.html">Next page</a>
</body></html>"#;

const NEXT: &str = r#"<!doctype html>
<html><head><title>Next Page</title></head><body><h1>Next Page</h1><p id="done">arrived</p></body></html>"#;

struct Browser {
    child: Child,
    port: u16,
    profile: PathBuf,
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.profile);
    }
}

fn chrome_binary() -> Option<PathBuf> {
    for key in ["CHROME_BIN", "COMPUTER_USE_CHROME", "GOOGLE_CHROME_SHIM"] {
        if let Some(value) = std::env::var_os(key) {
            let path = PathBuf::from(value);
            if path.exists() {
                return Some(path);
            }
        }
    }
    let candidates: &[&str] = if cfg!(target_os = "macos") {
        &[
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
        ]
    } else if cfg!(target_os = "windows") {
        &[
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        ]
    } else {
        &[
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
        ]
    };
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|path| path.exists())
}

/// Chromium, or `None` when this host has none and the suite may skip.
///
/// `COMPUTER_USE_REQUIRE_LIVE=1` (set by the `cdp-live` CI job, which installs
/// Chrome) turns the skip into a failure: a job that is supposed to exercise a
/// real browser must not pass while running nothing.
fn chrome_or_skip() -> Option<PathBuf> {
    let found = chrome_binary();
    if found.is_none() && std::env::var("COMPUTER_USE_REQUIRE_LIVE").ok().as_deref() == Some("1") {
        panic!("COMPUTER_USE_REQUIRE_LIVE=1 but no Chromium binary was found");
    }
    found
}

/// Skip the test when the host cannot run it.
///
/// Prints why, so a skipped run is visible in the log instead of looking like
/// a pass with no output.
macro_rules! skip_without_chrome {
    () => {
        if chrome_or_skip().is_none() {
            eprintln!("skipping live CDP test: no Chromium binary found");
            return;
        }
    };
}

fn start_browser() -> Option<Browser> {
    let executable = chrome_binary()?;
    let profile =
        std::env::temp_dir().join(format!("computer-use-cdp-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&profile);
    std::fs::create_dir_all(&profile).ok()?;
    let child = Command::new(&executable)
        .arg("--headless=new")
        .arg("--remote-debugging-port=0")
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("--remote-allow-origins=*")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-background-networking")
        .arg("--disable-extensions")
        .arg("--disable-default-apps")
        .arg("about:blank")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let port_file = profile.join("DevToolsActivePort");
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(&port_file) {
            if let Some(port) = text
                .lines()
                .next()
                .and_then(|line| line.trim().parse::<u16>().ok())
            {
                return Some(Browser {
                    child,
                    port,
                    profile,
                });
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

fn start_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
    let port = listener.local_addr().expect("listener addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buffer = [0u8; 8192];
            let read = stream.read(&mut buffer).unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..read]);
            let path = request.split_whitespace().nth(1).unwrap_or("/");
            let body = match path {
                "/" | "/index.html" => INDEX,
                "/next.html" => NEXT,
                _ => "<!doctype html><title>404</title><h1>404</h1>",
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    port
}

struct Fixture {
    browser: Browser,
    server_port: u16,
    cache: PathBuf,
    lock: Mutex<()>,
}

fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Option<Fixture>> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let browser = start_browser()?;
            let cache = std::env::temp_dir().join(format!(
                "computer-use-cdp-live-cache-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&cache);
            std::fs::create_dir_all(&cache).ok()?;
            Some(Fixture {
                browser,
                server_port: start_server(),
                cache,
                lock: Mutex::new(()),
            })
        })
        .as_ref()
        .expect("fixture")
}

fn run_cdp(args: &[&str]) -> Value {
    let fixture = fixture();
    let mut command = Command::new(bin());
    command
        .args(["--cdp", "--cdp-url"])
        .arg(format!("http://127.0.0.1:{}", fixture.browser.port))
        .args(args)
        .env("XDG_CACHE_HOME", &fixture.cache)
        .env("XDG_CONFIG_HOME", &fixture.cache)
        .env("XDG_STATE_HOME", &fixture.cache)
        .env_remove("COMPUTER_USE_CONFIRM_CMD");
    let output = command.output().expect("spawn computer-use");
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "response is not JSON: {error}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn base_url() -> String {
    format!("http://127.0.0.1:{}/", fixture().server_port)
}

#[test]
fn tabs_contract_uses_kind() {
    skip_without_chrome!();
    let fixture = fixture();
    let _guard = fixture
        .lock
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let response = run_cdp(&["browser", "tabs"]);
    assert_eq!(response["ok"], true, "{response}");
    let tabs = response["tabs"].as_array().expect("tabs array");
    assert!(!tabs.is_empty(), "{response}");
    for tab in tabs {
        assert!(tab.get("kind").is_some(), "missing kind: {tab}");
        assert!(tab.get("target_id").is_some(), "missing target_id: {tab}");
    }
}

#[test]
fn navigate_snapshot_and_click_by_ref() {
    skip_without_chrome!();
    let fixture = fixture();
    let _guard = fixture
        .lock
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let url = base_url();
    let navigate = run_cdp(&[
        "--session",
        "live-nav",
        "browser",
        "navigate",
        "--url",
        &url,
        "--wait",
        "load",
    ]);
    assert_eq!(navigate["ok"], true, "{navigate}");

    let snapshot = run_cdp(&["--session", "live-nav", "browser", "snapshot"]);
    assert_eq!(snapshot["ok"], true, "{snapshot}");
    let refs = snapshot["refs"].as_object().expect("refs object");
    assert!(!refs.is_empty(), "{snapshot}");

    // Find the button ref from the rendered text.
    let text = snapshot["text"].as_str().unwrap_or_default();
    let line = text
        .lines()
        .find(|line| line.contains("button \"Say hello\""))
        .ok_or_else(|| panic!("button not in snapshot: {text}"))
        .unwrap();
    let ref_id = line
        .split("ref=")
        .nth(1)
        .and_then(|tail| tail.split_whitespace().next())
        .expect("ref id")
        .to_string();

    let click = run_cdp(&[
        "--session",
        "live-nav",
        "browser",
        "click",
        "--ref",
        &ref_id,
        "--wait",
        "none",
    ]);
    assert_eq!(click["ok"], true, "{click}");
    let out = run_cdp(&[
        "--session",
        "live-nav",
        "browser",
        "text",
        "--selector",
        "#out",
    ]);
    assert_eq!(out["text"], "clicked", "{out}");
}

#[test]
fn type_and_secure_field_refusal() {
    skip_without_chrome!();
    let fixture = fixture();
    let _guard = fixture
        .lock
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let url = base_url();
    let navigate = run_cdp(&[
        "--session",
        "live-type",
        "browser",
        "navigate",
        "--url",
        &url,
        "--wait",
        "load",
    ]);
    assert_eq!(navigate["ok"], true, "{navigate}");
    let snapshot = run_cdp(&["--session", "live-type", "browser", "snapshot"]);
    let text = snapshot["text"].as_str().unwrap_or_default().to_string();
    let name_ref = ref_for(&text, "textbox \"Name\"");
    let password_ref = ref_for(&text, "textbox \"Password\"");

    let typed = run_cdp(&[
        "--session",
        "live-type",
        "browser",
        "type",
        "--ref",
        &name_ref,
        "--text",
        "Maria",
        "--clear",
    ]);
    assert_eq!(typed["ok"], true, "{typed}");
    let read = run_cdp(&[
        "--session",
        "live-type",
        "browser",
        "text",
        "--selector",
        "#name",
    ]);
    assert_eq!(read["text"], "Maria", "{read}");

    let refused = run_cdp(&[
        "--session",
        "live-type",
        "browser",
        "type",
        "--ref",
        &password_ref,
        "--text",
        "secret",
    ]);
    assert_eq!(refused["ok"], false, "{refused}");
    assert_eq!(refused["error"]["code"], "policy_denied", "{refused}");
}

#[test]
fn browser_batch_runs_steps_in_one_call() {
    skip_without_chrome!();
    let fixture = fixture();
    let _guard = fixture
        .lock
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let url = base_url();
    let steps = format!(
        r##"[{{"action":"browser_navigate","url":{url:?},"wait":"load"}},{{"action":"browser_click","selector":"#hello","wait":"none"}},{{"action":"browser_text","selector":"#out"}}]"##
    );
    let response = run_cdp(&["browser", "batch", &steps]);
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["count"], 3, "{response}");
    assert_eq!(response["failed"], 0, "{response}");
    assert_eq!(
        response["results"][2]["result"]["text"], "clicked",
        "{response}"
    );
}

#[test]
fn agent_marker_is_visible() {
    skip_without_chrome!();
    let fixture = fixture();
    let _guard = fixture
        .lock
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let url = base_url();
    let response = run_cdp(&["browser", "navigate", "--url", &url, "--wait", "load"]);
    assert_eq!(response["ok"], true, "{response}");
    let response = run_cdp(&[
        "--agent-overlay",
        "on",
        "browser",
        "eval",
        "--eval",
        "(() => { const icon = document.querySelector('link[rel~=icon]'); return {host: document.getElementById('__cu-agent-host') !== null, title: document.title, icon: icon ? icon.href : null}; })()",
    ]);
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["result"]["host"], true, "{response}");
    assert!(
        response["result"]["title"]
            .as_str()
            .unwrap_or_default()
            .starts_with('\u{1F916}'),
        "title was not prefixed: {response}"
    );
    assert!(
        response["result"]["icon"]
            .as_str()
            .unwrap_or_default()
            .starts_with("data:image/svg+xml"),
        "favicon was not replaced: {response}"
    );
}

#[test]
fn blocked_url_is_refused_before_connecting() {
    skip_without_chrome!();
    let fixture = fixture();
    let _guard = fixture
        .lock
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let policy = fixture.cache.join("live-policy.toml");
    std::fs::write(
        &policy,
        "version = 1\n[browser]\nblocked_urls = [\"http://127.0.0.1:*/blocked*\"]\n",
    )
    .expect("policy file");
    let response = run_cdp(&[
        "--policy",
        policy.to_str().unwrap(),
        "browser",
        "navigate",
        "--url",
        "http://127.0.0.1:9/blocked.html",
    ]);
    assert_eq!(response["ok"], false, "{response}");
    assert_eq!(response["error"]["code"], "blocked_url", "{response}");
}

/// Press, hover, scroll, select, and check against a live page.
#[test]
fn press_hover_scroll_select_and_check() {
    skip_without_chrome!();
    let fixture = fixture();
    let _guard = fixture
        .lock
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let url = base_url();
    let nav = run_cdp(&["browser", "navigate", "--url", &url, "--wait", "load"]);
    assert_eq!(nav["ok"], true, "{nav}");

    let hover = run_cdp(&["browser", "hover", "--selector", "#hello"]);
    assert_eq!(hover["ok"], true, "{hover}");

    let page_press = run_cdp(&["browser", "press", "--keys", "Tab"]);
    assert_eq!(page_press["ok"], true, "{page_press}");
    let element_press = run_cdp(&["browser", "press", "--selector", "#name", "--keys", "a"]);
    assert_eq!(element_press["ok"], true, "{element_press}");

    let page_scroll = run_cdp(&["browser", "scroll", "--dy", "100"]);
    assert_eq!(page_scroll["ok"], true, "{page_scroll}");
    let box_scroll = run_cdp(&[
        "browser",
        "scroll",
        "--selector",
        "#scrollbox",
        "--dy",
        "50",
    ]);
    assert_eq!(box_scroll["ok"], true, "{box_scroll}");

    let by_value = run_cdp(&[
        "browser",
        "select",
        "--selector",
        "#country",
        "--value",
        "pt",
    ]);
    assert_eq!(by_value["ok"], true, "{by_value}");
    let by_label = run_cdp(&[
        "browser",
        "select",
        "--selector",
        "#country",
        "--label",
        "Brazil",
    ]);
    assert_eq!(by_label["ok"], true, "{by_label}");
    let selected = run_cdp(&[
        "browser",
        "eval",
        "--eval",
        "document.getElementById('country').value",
    ]);
    assert_eq!(selected["result"], "br", "{selected}");

    let checked = run_cdp(&["browser", "check", "--selector", "#agree"]);
    assert_eq!(checked["ok"], true, "{checked}");
    let unchecked = run_cdp(&["browser", "check", "--selector", "#agree", "--unchecked"]);
    assert_eq!(unchecked["ok"], true, "{unchecked}");
    let state = run_cdp(&[
        "browser",
        "eval",
        "--eval",
        "document.getElementById('agree').checked",
    ]);
    assert_eq!(state["result"], false, "{state}");
}

/// Upload, screenshot, eval, dialogs, and tab lifecycle.
#[test]
fn upload_screenshot_eval_dialog_and_tabs() {
    skip_without_chrome!();
    let fixture = fixture();
    let _guard = fixture
        .lock
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let url = base_url();
    let nav = run_cdp(&["browser", "navigate", "--url", &url, "--wait", "load"]);
    assert_eq!(nav["ok"], true, "{nav}");

    let upload_path = fixture.cache.join("upload.txt");
    std::fs::write(&upload_path, "hello").expect("upload fixture");
    let upload = run_cdp(&[
        "browser",
        "upload",
        "--selector",
        "#file",
        "--file",
        upload_path.to_str().unwrap(),
    ]);
    assert_eq!(upload["ok"], true, "{upload}");
    let count = run_cdp(&[
        "browser",
        "eval",
        "--eval",
        "document.getElementById('file').files.length",
    ]);
    assert_eq!(count["result"], 1, "{count}");

    let shot_path = fixture.cache.join("live-shot.png");
    let shot = run_cdp(&[
        "browser",
        "screenshot",
        "--output",
        shot_path.to_str().unwrap(),
        "--full-page",
    ]);
    assert_eq!(shot["ok"], true, "{shot}");
    assert!(
        std::fs::metadata(&shot_path)
            .map(|meta| meta.len() > 0)
            .unwrap_or(false),
        "{shot}"
    );
    let _ = std::fs::remove_file(&shot_path);

    let inline = run_cdp(&["browser", "screenshot", "--base64"]);
    assert_eq!(inline["ok"], true, "{inline}");
    assert!(
        inline["base64"]
            .as_str()
            .is_some_and(|data| !data.is_empty()),
        "{inline}"
    );
    let jpeg_path = fixture.cache.join("live-shot.jpg");
    let jpeg = run_cdp(&[
        "browser",
        "screenshot",
        "--format",
        "jpeg",
        "--quality",
        "60",
        "--output",
        jpeg_path.to_str().unwrap(),
    ]);
    assert_eq!(jpeg["ok"], true, "{jpeg}");
    assert_eq!(jpeg["format"], "jpg", "{jpeg}");
    let _ = std::fs::remove_file(&jpeg_path);

    let eval = run_cdp(&["browser", "eval", "--eval", "1+1"]);
    assert_eq!(eval["result"], 2, "{eval}");
    let promise = run_cdp(&[
        "browser",
        "eval",
        "--eval",
        "Promise.resolve(3)",
        "--await-promise",
    ]);
    assert_eq!(promise["result"], 3, "{promise}");

    // The explicit dialog command goes through the raw CDP rescue path.
    // With no dialog open Chrome may accept it as a no-op or report a clean
    // error; either way it must be a well-formed response.
    let dialog = run_cdp(&["browser", "dialog", "--accept"]);
    assert!(dialog.get("ok").is_some(), "{dialog}");
    if dialog["ok"] == false {
        assert_eq!(dialog["error"]["code"], "execution_failed", "{dialog}");
    }

    let before = run_cdp(&["browser", "tabs"]);
    let before_count = before["tabs"].as_array().expect("tabs").len();
    let opened = run_cdp(&["browser", "tab-new", "--url", &url]);
    assert_eq!(opened["ok"], true, "{opened}");
    let new_target = opened["target_id"].as_str().expect("target id").to_string();
    let after = run_cdp(&["browser", "tabs"]);
    assert_eq!(
        after["tabs"].as_array().expect("tabs").len(),
        before_count + 1,
        "{after}"
    );
    let closed = run_cdp(&["browser", "tab-close", "--target", &new_target]);
    assert_eq!(closed["closed"], true, "{closed}");
}

/// Best-effort cleanup for a managed browser started by the test.
struct ManagedQuit {
    session: String,
    cache: std::path::PathBuf,
}

impl Drop for ManagedQuit {
    fn drop(&mut self) {
        let mut command = Command::new(bin());
        command
            .args([
                "--cdp",
                "browser",
                "quit",
                "--session",
                &self.session,
                "--purge",
            ])
            .env("XDG_CACHE_HOME", &self.cache)
            .env("XDG_CONFIG_HOME", &self.cache)
            .env("XDG_STATE_HOME", &self.cache)
            .env_remove("COMPUTER_USE_CONFIRM_CMD");
        let _ = command.output();
    }
}

/// Start a managed Chromium, see it in `sessions`, and quit it.
#[test]
fn managed_launch_sessions_and_quit() {
    skip_without_chrome!();
    let fixture = fixture();
    let _guard = fixture
        .lock
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let chrome = chrome_or_skip().expect("chrome");

    let missing = run_cdp(&[
        "browser",
        "launch",
        "--browser",
        "/definitely/missing/chrome",
    ]);
    assert_eq!(missing["error"]["code"], "invalid_arguments", "{missing}");

    let launched = run_cdp(&[
        "browser",
        "launch",
        "--headless",
        "--browser",
        chrome.to_str().unwrap(),
        "--url",
        &base_url(),
    ]);
    assert_eq!(launched["ok"], true, "{launched}");
    let session = launched["session"]
        .as_str()
        .expect("session id")
        .to_string();
    let _quit = ManagedQuit {
        session: session.clone(),
        cache: fixture.cache.clone(),
    };

    let sessions = run_cdp(&["browser", "sessions"]);
    assert_eq!(sessions["ok"], true, "{sessions}");
    assert!(
        sessions["sessions"]
            .as_array()
            .expect("sessions")
            .iter()
            .any(|entry| entry["session"] == session.as_str()),
        "{sessions}"
    );

    let quit = run_cdp(&["browser", "quit", "--session", &session, "--purge"]);
    assert_eq!(quit["ok"], true, "{quit}");
    assert_eq!(quit["session"], session.as_str(), "{quit}");

    // A session that was never started is a no-op, not an error.
    let unknown = run_cdp(&["browser", "quit", "--session", "no-such-session"]);
    assert_eq!(unknown["ok"], true, "{unknown}");
    assert_eq!(unknown["killed"], false, "{unknown}");
}

/// Navigation history, reload, the wait forms, and activate.
#[test]
fn navigation_history_reload_waits_and_activate() {
    skip_without_chrome!();
    let fixture = fixture();
    let _guard = fixture
        .lock
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let url = base_url();
    // A fresh tab gives this test its own history, independent of whatever
    // page the other tests left behind in the shared browser.
    let nav = run_cdp(&[
        "browser",
        "navigate",
        "--url",
        &url,
        "--new-tab",
        "--wait",
        "load",
    ]);
    assert_eq!(nav["ok"], true, "{nav}");
    let target = nav["target_id"].as_str().expect("target id").to_string();

    let selector_wait = run_cdp(&[
        "browser",
        "wait",
        "--target",
        &target,
        "--wait",
        "selector:#out",
    ]);
    assert_eq!(selector_wait["ok"], true, "{selector_wait}");

    // A same-page click with a load wait exercises that arm without relying on
    // a click-created history entry.
    let clicked = run_cdp(&[
        "browser",
        "click",
        "--target",
        &target,
        "--selector",
        "#hello",
        "--wait",
        "load",
    ]);
    assert_eq!(clicked["ok"], true, "{clicked}");

    // Navigate explicitly so the history entry is deterministic under load.
    let next = run_cdp(&[
        "browser",
        "navigate",
        "--target",
        &target,
        "--url",
        &format!("{url}next.html"),
        "--wait",
        "load",
    ]);
    assert_eq!(next["ok"], true, "{next}");

    let url_wait = run_cdp(&[
        "browser",
        "wait",
        "--target",
        &target,
        "--wait",
        "url:*next*",
    ]);
    assert_eq!(url_wait["ok"], true, "{url_wait}");

    let idle = run_cdp(&[
        "browser",
        "wait",
        "--target",
        &target,
        "--wait",
        "networkidle",
        "--timeout-ms",
        "5000",
    ]);
    assert_eq!(idle["ok"], true, "{idle}");

    let back = run_cdp(&["browser", "back", "--target", &target]);
    assert_eq!(back["ok"], true, "{back}");
    let forward = run_cdp(&["browser", "forward", "--target", &target]);
    assert_eq!(forward["ok"], true, "{forward}");
    let reload = run_cdp(&[
        "browser",
        "reload",
        "--target",
        &target,
        "--wait",
        "domcontentloaded",
    ]);
    assert_eq!(reload["ok"], true, "{reload}");
    let activate = run_cdp(&["browser", "activate", "--target", &target]);
    assert_eq!(activate["ok"], true, "{activate}");

    let closed = run_cdp(&["browser", "tab-close", "--target", &target]);
    assert_eq!(closed["closed"], true, "{closed}");
}

/// Selector misses, stale refs, batch failures, and a blocked current page.
#[test]
fn selector_ref_and_batch_errors_are_reported() {
    skip_without_chrome!();
    let fixture = fixture();
    let _guard = fixture
        .lock
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let url = base_url();
    let nav = run_cdp(&["browser", "navigate", "--url", &url, "--wait", "load"]);
    assert_eq!(nav["ok"], true, "{nav}");

    let missing = run_cdp(&[
        "browser",
        "click",
        "--selector",
        "#does-not-exist",
        "--wait",
        "none",
    ]);
    assert_eq!(missing["error"]["code"], "element_not_found", "{missing}");

    let stale = run_cdp(&["browser", "press", "--ref", "e999", "--keys", "a"]);
    assert_eq!(stale["error"]["code"], "stale_element", "{stale}");

    let steps = r##"[{"action":"browser_click","selector":"#does-not-exist","wait":"none"},{"action":"browser_eval","script":"1"}]"##;
    let batch = run_cdp(&["browser", "batch", steps]);
    assert_eq!(batch["ok"], true, "{batch}");
    assert_eq!(batch["failed"], 1, "{batch}");
    assert_eq!(batch["stopped_at"], 0, "{batch}");
    assert_eq!(batch["count"], 1, "{batch}");

    let policy = fixture.cache.join("block-current.toml");
    std::fs::write(
        &policy,
        "version = 1\n[browser]\nblocked_urls = [\"http://127.0.0.1:*/*\"]\n",
    )
    .expect("policy file");
    let blocked = run_cdp(&[
        "--policy",
        policy.to_str().unwrap(),
        "browser",
        "click",
        "--selector",
        "#hello",
        "--wait",
        "none",
    ]);
    assert_eq!(blocked["error"]["code"], "blocked_url", "{blocked}");
}

/// Snapshot refs reach every element command, not just click and type.
#[test]
fn snapshot_refs_reach_element_commands() {
    skip_without_chrome!();
    let fixture = fixture();
    let _guard = fixture
        .lock
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let url = base_url();
    let nav = run_cdp(&["browser", "navigate", "--url", &url, "--wait", "load"]);
    assert_eq!(nav["ok"], true, "{nav}");

    // The snapshot cache is keyed by --session; every later command must use
    // the same one for --ref to resolve.
    let snapshot = run_cdp(&["--session", "live-refs", "browser", "snapshot"]);
    assert_eq!(snapshot["ok"], true, "{snapshot}");
    let tree = snapshot["text"].as_str().expect("snapshot text");
    let hello = ref_for(tree, "Say hello");
    let name = ref_for(tree, "Name");
    let country = ref_for(tree, "Country");
    let agree = ref_for(tree, "Agree");
    let file = ref_for(tree, "File");

    let pressed = run_cdp(&[
        "--session",
        "live-refs",
        "browser",
        "press",
        "--ref",
        &hello,
        "--keys",
        "Enter",
    ]);
    assert_eq!(pressed["ok"], true, "{pressed}");
    let typed = run_cdp(&[
        "--session",
        "live-refs",
        "browser",
        "type",
        "--ref",
        &name,
        "--text",
        "hello",
    ]);
    assert_eq!(typed["ok"], true, "{typed}");
    let hovered = run_cdp(&[
        "--session",
        "live-refs",
        "browser",
        "hover",
        "--ref",
        &hello,
    ]);
    assert_eq!(hovered["ok"], true, "{hovered}");
    let scrolled = run_cdp(&[
        "--session",
        "live-refs",
        "browser",
        "scroll",
        "--ref",
        &hello,
        "--dy",
        "10",
    ]);
    assert_eq!(scrolled["ok"], true, "{scrolled}");
    let selected = run_cdp(&[
        "--session",
        "live-refs",
        "browser",
        "select",
        "--ref",
        &country,
        "--value",
        "br",
    ]);
    assert_eq!(selected["ok"], true, "{selected}");
    let checked = run_cdp(&[
        "--session",
        "live-refs",
        "browser",
        "check",
        "--ref",
        &agree,
    ]);
    assert_eq!(checked["ok"], true, "{checked}");

    let upload_path = fixture.cache.join("upload-ref.txt");
    std::fs::write(&upload_path, "x").expect("upload fixture");
    let uploaded = run_cdp(&[
        "--session",
        "live-refs",
        "browser",
        "upload",
        "--ref",
        &file,
        "--file",
        upload_path.to_str().unwrap(),
    ]);
    assert_eq!(uploaded["ok"], true, "{uploaded}");
}

fn ref_for(text: &str, needle: &str) -> String {
    let line = text
        .lines()
        .find(|line| line.contains(needle) && line.contains("ref="))
        .unwrap_or_else(|| panic!("{needle} with ref not in snapshot: {text}"));
    line.split("ref=")
        .nth(1)
        .and_then(|tail| tail.split_whitespace().next())
        .expect("ref id")
        .to_string()
}

/// New-tab navigation, ambiguous target resolution, blocked tab rows, and a
/// blocked activate.
#[test]
fn new_tab_ambiguous_targets_and_blocked_rows() {
    skip_without_chrome!();
    let fixture = fixture();
    let _guard = fixture
        .lock
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let url = base_url();
    let nav = run_cdp(&[
        "browser",
        "navigate",
        "--url",
        &url,
        "--new-tab",
        "--wait",
        "load",
    ]);
    assert_eq!(nav["ok"], true, "{nav}");
    let target = nav["target_id"].as_str().expect("target id").to_string();

    let second = run_cdp(&["browser", "tab-new", "--url", &url]);
    assert_eq!(second["ok"], true, "{second}");
    let second_target = second["target_id"]
        .as_str()
        .expect("second target")
        .to_string();

    // With more than one page and no --target, the command refuses to guess.
    let ambiguous = run_cdp(&["browser", "snapshot"]);
    assert_eq!(
        ambiguous["error"]["code"], "invalid_arguments",
        "{ambiguous}"
    );
    assert!(ambiguous["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("multiple page targets"));

    let policy = fixture.cache.join("blocked-tabs.toml");
    std::fs::write(
        &policy,
        "version = 1\n[browser]\nblocked_urls = [\"http://127.0.0.1:*/*\"]\n",
    )
    .expect("policy file");
    let tabs = run_cdp(&["--policy", policy.to_str().unwrap(), "browser", "tabs"]);
    assert_eq!(tabs["ok"], true, "{tabs}");
    assert!(
        tabs["tabs"]
            .as_array()
            .expect("tabs")
            .iter()
            .any(|tab| tab["blocked"] == true
                && tab["url"] == "<blocked>"
                && tab["title"].is_null()),
        "{tabs}"
    );

    let activate = run_cdp(&[
        "--policy",
        policy.to_str().unwrap(),
        "browser",
        "activate",
        "--target",
        &target,
    ]);
    assert_eq!(activate["error"]["code"], "blocked_url", "{activate}");

    let _ = run_cdp(&["browser", "tab-close", "--target", &second_target]);
    let _ = run_cdp(&["browser", "tab-close", "--target", &target]);
}

//! Live CDP tests against a real Chromium.
//!
//! The suite starts one headless browser for the file and one in-process HTTP
//! server, then runs the compiled binary against them. It is skipped when no
//! Chromium executable can be found, so `cargo test` stays green on a machine
//! without a browser. CI sets `CHROME_BIN` (or installs Chrome) and therefore
//! always runs it.

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
    if chrome_binary().is_none() {
        eprintln!("skipping live CDP test: no Chromium binary found");
        return;
    }
    let fixture = fixture();
    let _guard = fixture.lock.lock().expect("test lock");
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
    if chrome_binary().is_none() {
        eprintln!("skipping live CDP test: no Chromium binary found");
        return;
    }
    let fixture = fixture();
    let _guard = fixture.lock.lock().expect("test lock");
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
    if chrome_binary().is_none() {
        eprintln!("skipping live CDP test: no Chromium binary found");
        return;
    }
    let fixture = fixture();
    let _guard = fixture.lock.lock().expect("test lock");
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
    if chrome_binary().is_none() {
        eprintln!("skipping live CDP test: no Chromium binary found");
        return;
    }
    let fixture = fixture();
    let _guard = fixture.lock.lock().expect("test lock");
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
    if chrome_binary().is_none() {
        eprintln!("skipping live CDP test: no Chromium binary found");
        return;
    }
    let fixture = fixture();
    let _guard = fixture.lock.lock().expect("test lock");
    let url = base_url();
    let response = run_cdp(&["browser", "navigate", "--url", &url, "--wait", "load"]);
    assert_eq!(response["ok"], true, "{response}");
    let response = run_cdp(&[
        "--agent-overlay",
        "on",
        "browser",
        "eval",
        "--eval",
        "(() => ({host: document.getElementById('__cu-agent-host') !== null, title: document.title}))()",
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
}

#[test]
fn blocked_url_is_refused_before_connecting() {
    if chrome_binary().is_none() {
        eprintln!("skipping live CDP test: no Chromium binary found");
        return;
    }
    let fixture = fixture();
    let _guard = fixture.lock.lock().expect("test lock");
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

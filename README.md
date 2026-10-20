# computer-use

Cross-platform desktop automation for agents: screenshots, mouse, keyboard, scroll, displays, and a best-effort accessibility snapshot. One binary prints a single JSON object for every action. The same actions are available as a Rust library.

The [smart-computer-use](https://github.com/lindoelio/smart-computer-use) agent skill shells out to this binary.

## Install

### GitHub Releases

Download a build for your platform from [GitHub Releases](https://github.com/lindoelio/computer-use/releases):

| Asset | Platform |
| --- | --- |
| `computer-use-linux-x64.tar.gz` | Linux x86_64 |
| `computer-use-macos-arm64.tar.gz` | macOS Apple silicon |
| `computer-use-windows-x64.zip` | Windows x86_64 |

Each archive contains the `computer-use` binary (`computer-use.exe` on Windows). A matching `<asset>.sha256` file and a combined `SHA256SUMS` file are attached to the release.

```bash
# macOS (Apple silicon)
curl -fsSL -o computer-use-macos-arm64.tar.gz \
  https://github.com/lindoelio/computer-use/releases/latest/download/computer-use-macos-arm64.tar.gz
tar -xzf computer-use-macos-arm64.tar.gz
sudo install -m 755 computer-use /usr/local/bin/computer-use

# Linux x86_64
curl -fsSL -o computer-use-linux-x64.tar.gz \
  https://github.com/lindoelio/computer-use/releases/latest/download/computer-use-linux-x64.tar.gz
tar -xzf computer-use-linux-x64.tar.gz
sudo install -m 755 computer-use /usr/local/bin/computer-use
```

Check the archive before you install it:

```bash
shasum -a 256 -c computer-use-macos-arm64.tar.gz.sha256
# or, on Linux:
sha256sum -c computer-use-linux-x64.tar.gz.sha256
```

### cargo

```bash
cargo install computer-use
```

Rust 1.81 or newer is required. On Linux, install the native libraries listed under [Permissions](#permissions) before building.

## Usage

Every action writes one JSON object to stdout and a trailing newline.

```json
{"ok":true,"waited_ms":10}
```

```json
{"ok":false,"error":{"code":"invalid_arguments","message":"wait requires --ms or --seconds"}}
```

A failed action exits with status 1. A bad command line (missing flags, unknown arguments) exits with status 2 and still prints JSON. `--help` and `--version` stay human-readable and exit 0.

`--pretty` pretty-prints one-shot JSON. `serve` and `--json` always print compact JSON so each response stays on one line.

```bash
computer-use --help
computer-use --version
computer-use wait --ms 10
computer-use --pretty wait --ms 10
```

### screenshot

Capture the screen, or a logical region, and write a PNG or JPEG. The default path is under the system temp directory. Coordinates are logical points, the same space as `click` and `move`. On a Retina display the image pixel size can be `scale` times the logical size.

```bash
computer-use screenshot
computer-use screenshot --output /tmp/screen.png --region 0,0,800,600
computer-use screenshot --format jpeg --max-width 1280 --base64
```

`--format` accepts `png` (default), `jpeg`, or `jpg`. `--max-width N` scales the image down so its width is at most N pixels. `--base64` adds a standard (unwrapped) base64 `data` field.

```json
{"ok":true,"path":"/tmp/computer-use-123-456.png","width":800,"height":600,"scale":2.0,"format":"png"}
```

### click

Move the pointer to a logical point and click. `--button` is `left` (default), `right`, or `middle`. `--double` clicks twice.

```bash
computer-use click --x 120 --y 240
computer-use click --x 120 --y 240 --button right --double
```

### move

Move the pointer without clicking.

```bash
computer-use move --x 400 --y 300
```

### type

Type Unicode text into the focused control.

```bash
computer-use type --text "hello"
```

### key

Press a key combination. The last segment is the key. Earlier segments are modifiers.

| Token | Meaning |
| --- | --- |
| `cmd`, `command` | Command (macOS). Sent as the platform meta key. |
| `meta`, `super` | Meta / Super |
| `ctrl`, `control` | Control |
| `alt`, `option` | Alt / Option |
| `shift` | Shift |

Named keys: `enter` / `return`, `tab`, `esc` / `escape`, `space`, `left` / `right` / `up` / `down` (and `arrow` aliases), `f1`–`f12`, `backspace`, `delete`, `home`, `end`, `pageup` / `pagedown` (and `page_up` / `page_down`). A single character is that character, case preserved (`shift+A`).

```bash
computer-use key --keys "cmd+shift+4"
computer-use key --keys "ctrl+c"
computer-use key --keys "alt+tab"
```

### scroll

Scroll the mouse wheel. Positive `--dy` scrolls down. Positive `--dx` scrolls right. Pass `--x` and `--y` together to move the pointer first.

Use `--dx` and `--dy`, or `--direction` (`up`, `down`, `left`, `right`) with `--amount` (default 1). Do not mix the two forms.

```bash
computer-use scroll --dy 3
computer-use scroll --dx -2 --dy 1
computer-use scroll --direction down --amount 3
computer-use scroll --x 400 --y 300 --direction up --amount 2
```

### get-state

Report screen size and displays, cursor position, the frontmost app and window, and a best-effort accessibility tree. The command does not panic when a subsystem is unavailable. Missing data is `null`, and `note` explains why. `get_state` is an alias.

```bash
computer-use get-state
computer-use get_state
```

```json
{
  "ok": true,
  "platform": "macos",
  "displays": [{"id": 1, "name": "Built-in", "x": 0, "y": 0, "width": 1512, "height": 982, "scale": 2.0, "is_primary": true}],
  "screen": {"id": 1, "name": "Built-in", "width": 1512, "height": 982, "scale": 2.0, "x": 0, "y": 0},
  "cursor": {"x": 400, "y": 300},
  "frontmost_app": "Finder",
  "frontmost_window": "Desktop",
  "accessibility": null,
  "note": "accessibility permission is required"
}
```

### wait

Sleep, then return `waited_ms`. `--ms` and `--seconds` are added when both are set. At least one is required. Fractions are allowed for `--seconds`.

```bash
computer-use wait --ms 250
computer-use wait --seconds 0.5
computer-use wait --ms 100 --seconds 1
```

### displays

List connected displays without capturing pixels.

```bash
computer-use displays
```

### JSON mode

`serve` and `--json` read newline-delimited JSON from stdin and write one JSON response per line. This is a stdin protocol, not a TCP server. The process exits 0 on a clean end of input. It exits non-zero only when stdin or stdout fails. An individual action can still be `{"ok":false,...}`.

`exec` runs one JSON action from an argument and honors `--pretty`.

```bash
printf '%s\n' '{"id":1,"action":"wait","ms":10}' | computer-use serve
printf '%s\n' '{"id":1,"action":"click","x":10,"y":20}' | computer-use --json
computer-use exec '{"action":"wait","ms":10}'
computer-use --pretty exec '{"id":"a","action":"displays"}'
```

Requests are objects with an `action` tag and optional `id`. The `id` is echoed on the response. Unknown fields that belong to the action are rejected.

| `action` | Fields |
| --- | --- |
| `screenshot` | `output`, `region` (`"x,y,w,h"` or `{x,y,width,height}`), `format` (`png` or `jpeg`), `max_width`, `base64` |
| `click` | `x`, `y`, `button` (`left`, `right`, `middle`), `double` |
| `double_click` | `x`, `y`, `button` |
| `move` | `x`, `y`. Alias: `move_mouse` |
| `type` | `text` |
| `key` | `keys` |
| `scroll` | `x`, `y`, `dx` (alias `scroll_x`), `dy` (alias `scroll_y`), `direction`, `amount` |
| `get_state` | none. Alias: `get-state` |
| `wait` | `ms`, `seconds` |
| `displays` | none |

```json
{"id":1,"action":"click","x":10,"y":20,"button":"left"}
{"id":2,"action":"key","keys":"cmd+shift+4"}
{"id":3,"action":"scroll","direction":"down","amount":3}
{"id":4,"action":"screenshot","region":"0,0,800,600","format":"png"}
{"action":"get_state"}
```

Error `code` values are `invalid_arguments`, `execution_failed`, `io_error`, and `unsupported`.

## Library

```rust
use computer_use::{dispatch, Action};

let response = dispatch(Action::Wait {
    ms: Some(0),
    seconds: None,
});
assert!(response.ok);
```

`DesktopBackend` (also exported as `Backend`) is the platform trait. `create_desktop_backend()` returns the backend for the current operating system. `dispatch` and `dispatch_with` share the path used by the CLI. `Action` and the JSON request parser use serde with `tag = "action"`. Errors use `thiserror` (`ComputerError`).

## Permissions

Input and capture need permission from the operating system. `get-state` still returns `ok: true` when a subsystem is denied: the missing field is `null` and `note` says why. The process does not prompt for macOS permission on startup (`open_prompt_to_get_permissions` is off). Grant permission in System Settings before you call click, type, key, or screenshot.

### macOS

- **Screen Recording** — System Settings → Privacy & Security → Screen Recording. Required for `screenshot` and display capture inside `get-state`.
- **Accessibility** — System Settings → Privacy & Security → Accessibility. Required for mouse, keyboard, and the accessibility tree (`osascript` / System Events).

### Windows

Run the process in a session that can send input (an interactive desktop, not Session 0).

- **Apps and windows** come from Win32 `EnumWindows`, `GetWindowTextW`, `GetWindowThreadProcessId`, and `QueryFullProcessImageNameW`. There is no PowerShell anywhere in the binary.
- **The accessibility tree** is the UI Automation control-view walker (`uiautomation` 0.25.1). `tree` reports `source: "uia"`, `--app pid:NNN` is a deterministic selector, and `GetForegroundWindow` is never used to resolve a window the caller already named.
- **Element actions** run through UIA control patterns (`Invoke`, `Value`, `Text`, `ScrollItem`, `ExpandCollapse`) in the background — tier `ax`, no cursor motion. `IsPassword` controls refuse `set_value` and `type --element`. When a pattern is missing, or its single call fails (some Win32 apps ignore `Invoke` until they are in front), the action is retried as tier `activate`: `SetForegroundWindow` plus enigo input, under the same `foreground.lock` as every other foreground path, with a `foreground required` warning on the result. `--require-background` refuses that fallback, and the P6 policy allowlist still applies.
- **Per-window capture** is one `Windows.Graphics.Capture` frame from the resolved `HWND` (`windows-capture` 2.0.1). If graphics capture cannot produce a frame, the desktop image is cropped instead and reported as `capture_source: "xcap"`, so covered pixels may be wrong.
- **OCR** is `Windows.Media.Ocr.OcrEngine` for the user-profile languages. If no recognizer language pack is installed, `ocr` returns `execution_failed` naming Settings → Apps → Optional features. `screenshot --marks` draws the tree frames or the OCR boxes.
- **`doctor`** reports `accessibility` (did `UIAutomation::new` succeed), plus best-effort `uiaccess` and `integrity` read from the process token with `OpenProcessToken` / `GetTokenInformation`. Neither is a required grant.

A corporate policy that blocks UI Automation or simulated input surfaces as `execution_failed` or a null field.

### Linux

The binary talks to X11 through `libxdo` (enigo) and captures through `xcap` (XCB, RandR, D-Bus). Wayland sessions are not a supported target. Install build packages on Debian/Ubuntu:

```bash
sudo apt-get install -y pkg-config libxdo-dev libx11-dev libxcb1-dev \
  libxrandr-dev libdbus-1-dev libxkbcommon-dev
```

`xdotool` on `PATH` improves the frontmost-window lookup. The accessibility tree is `null` with a note (AT-SPI is not wired). Grant the session access to the X display (`DISPLAY` set, and the same user that owns the session).

## Coordinates

Click, move, scroll, and screenshot regions use **logical** points. A display's `width`, `height`, `x`, and `y` are logical. `scale` is physical pixels divided by logical width (often `2.0` on Retina). A screenshot's `width` and `height` are image pixels, which can be larger than the logical region.

Scroll signs follow the platform wheel: positive `dy` is down, positive `dx` is right.

## Development

```bash
cargo fmt --all
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
```

Tests do not move the pointer, capture the screen, or require Screen Recording or Accessibility.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Copyright 2026 Lindoélio Lázaro.

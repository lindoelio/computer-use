//! Windows frontmost-window lookup.
//!
//! Input uses enigo (Win32 `SendInput`). The full UI Automation tree is not
//! collected. The foreground window title and process name come from a
//! short-lived PowerShell call and are best-effort.

use std::time::Duration;

use crate::doctor::{self, DoctorInput, DoctorOptions};
use crate::error::ComputerError;
use crate::target::{AppInfo, DoctorReport, LaunchResult, QuitResult, WindowList};
use crate::types::FrontmostInfo;

use super::unsupported;

use super::command::{run_timeout, stderr_snippet};

/// Windows desktop backend. Methods are implemented in the parent module.
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsDesktopBackend;

const SCRIPT: &str = r#"
$sig = @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public class ComputerUseWin {
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr hWnd, StringBuilder text, int count);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
}
'@
Add-Type -TypeDefinition $sig -ErrorAction Stop
$hwnd = [ComputerUseWin]::GetForegroundWindow()
$sb = New-Object System.Text.StringBuilder 1024
[void][ComputerUseWin]::GetWindowText($hwnd, $sb, $sb.Capacity)
$procId = [uint32]0
[void][ComputerUseWin]::GetWindowThreadProcessId($hwnd, [ref]$procId)
$proc = Get-Process -Id $procId -ErrorAction SilentlyContinue
$app = if ($proc) { $proc.ProcessName } else { "" }
Write-Output $app
Write-Output ($sb.ToString())
"#;

pub(crate) fn frontmost() -> FrontmostInfo {
    match run_timeout(
        "powershell",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            SCRIPT,
        ],
        Duration::from_secs(8),
    ) {
        Ok(output) if output.status.success() => split_frontmost(&output.stdout),
        Ok(output) => FrontmostInfo {
            note: Some(format!(
                "foreground window lookup failed: {}",
                stderr_snippet(&output.stderr)
            )),
            ..FrontmostInfo::default()
        },
        Err(error) => FrontmostInfo {
            note: Some(format!("foreground window unavailable: {error}")),
            ..FrontmostInfo::default()
        },
    }
}

fn split_frontmost(stdout: &[u8]) -> FrontmostInfo {
    let text = String::from_utf8_lossy(stdout);
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let app = lines.next().map(str::to_string);
    let window = lines.next().map(str::to_string);
    FrontmostInfo {
        app,
        window,
        note: None,
    }
}

pub(crate) fn inspect_accessibility(_app_name: Option<&str>) -> Result<String, ComputerError> {
    Err(ComputerError::Unsupported(
        "accessibility tree is not available on Windows in this build. foreground window title and process name are reported when Win32 can see them".into(),
    ))
}

pub(crate) fn list_apps(_all: bool) -> Result<Vec<AppInfo>, ComputerError> {
    Err(unsupported("list_apps"))
}

pub(crate) fn list_windows() -> Result<WindowList, ComputerError> {
    Err(unsupported("list_windows"))
}

pub(crate) fn launch(_app: &str) -> Result<LaunchResult, ComputerError> {
    Err(unsupported("launch"))
}

pub(crate) fn activate(_app: &str, _window: Option<&str>) -> Result<LaunchResult, ComputerError> {
    Err(unsupported("activate"))
}

pub(crate) fn quit(_app: &str, _force: bool) -> Result<QuitResult, ComputerError> {
    Err(unsupported("quit"))
}

pub(crate) fn tree(
    _query: &crate::tree::TreeQuery,
) -> Result<crate::tree::TreeReport, ComputerError> {
    Err(unsupported("tree"))
}

pub(crate) fn end_session(session: Option<&str>) -> Result<Vec<i32>, ComputerError> {
    if let Ok(pending) = crate::cache::pending_restores(session) {
        let pids: Vec<i32> = pending.into_iter().map(|(pid, _)| pid).collect();
        if !pids.is_empty() {
            let _ = crate::cache::clear_restores(session, &pids);
        }
    }
    Ok(Vec::new())
}

pub(crate) fn doctor(options: DoctorOptions) -> Result<DoctorReport, ComputerError> {
    let mut notes = vec![
        "accessibility and screen-capture probes land in P12".to_string(),
        "uiaccess and integrity are best-effort and are not required grants".to_string(),
    ];
    let (uiaccess, integrity, token_note) = read_token();
    if let Some(note) = token_note {
        notes.push(note);
    }
    let report = doctor::assemble(DoctorInput {
        path: doctor::current_executable(),
        uiaccess,
        integrity,
        notes,
        prompt_ignored: options.prompt,
        open_settings_ignored: options.open_settings,
        ..DoctorInput::default()
    });
    Ok(report)
}

/// Best-effort UIAccess and integrity. A failure leaves a null and a note.
fn read_token() -> (Option<bool>, Option<String>, Option<String>) {
    let (uiaccess, mut integrity, mut note) = match token_script() {
        Ok((uiaccess, integrity)) => (uiaccess, integrity, None),
        Err(error) => (None, None, Some(error)),
    };
    if integrity.is_none() {
        integrity = whoami_integrity();
    }
    if uiaccess.is_none() || integrity.is_none() {
        let detail = note.unwrap_or_else(|| "token fields were incomplete".to_string());
        note = Some(detail);
    }
    (uiaccess, integrity, note)
}

fn token_script() -> Result<(Option<bool>, Option<String>), String> {
    let path = std::env::temp_dir().join(format!("computer-use-token-{}.ps1", std::process::id()));
    std::fs::write(&path, TOKEN_SCRIPT)
        .map_err(|error| format!("could not write token script: {error}"))?;
    let result = run_timeout(
        "powershell",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-File",
            &path.to_string_lossy(),
        ],
        Duration::from_secs(8),
    );
    let _ = std::fs::remove_file(&path);
    let output = result?;
    if !output.status.success() {
        return Err(stderr_snippet(&output.stderr));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(doctor::parse_token_lines(&text))
}

fn whoami_integrity() -> Option<String> {
    let output = run_timeout("whoami", &["/groups"], Duration::from_secs(4)).ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    doctor::integrity_from_whoami(&text).map(str::to_string)
}

const TOKEN_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public class CuToken {
  const uint TOKEN_QUERY = 8;
  [DllImport("advapi32.dll", SetLastError=true)]
  static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
  [DllImport("advapi32.dll", SetLastError=true)]
  static extern bool GetTokenInformation(IntPtr token, int cls, IntPtr info, int len, out int ret);
  [DllImport("kernel32.dll")]
  static extern IntPtr GetCurrentProcess();
  [DllImport("kernel32.dll")]
  static extern bool CloseHandle(IntPtr handle);
  [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
  static extern bool ConvertSidToStringSid(IntPtr sid, out IntPtr text);
  [DllImport("kernel32.dll")]
  static extern IntPtr LocalFree(IntPtr handle);
  public static string Read() {
    IntPtr token;
    if (!OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, out token)) {
      return "uiaccess=\nintegrity=";
    }
    string ui = "";
    string level = "";
    try {
      int ret;
      IntPtr uiBuf = Marshal.AllocHGlobal(4);
      if (GetTokenInformation(token, 26, uiBuf, 4, out ret)) {
        ui = Marshal.ReadInt32(uiBuf) != 0 ? "true" : "false";
      }
      Marshal.FreeHGlobal(uiBuf);
      int size = 0;
      GetTokenInformation(token, 25, IntPtr.Zero, 0, out size);
      if (size > 0) {
        IntPtr buf = Marshal.AllocHGlobal(size);
        if (GetTokenInformation(token, 25, buf, size, out ret)) {
          IntPtr sid = Marshal.ReadIntPtr(buf);
          IntPtr text;
          if (sid != IntPtr.Zero && ConvertSidToStringSid(sid, out text)) {
            string sidText = Marshal.PtrToStringUni(text);
            if (text != IntPtr.Zero) LocalFree(text);
            level = Rid(sidText);
          }
        }
        Marshal.FreeHGlobal(buf);
      }
    } finally {
      CloseHandle(token);
    }
    return "uiaccess=" + ui + "\nintegrity=" + level;
  }
  static string Rid(string sid) {
    if (sid == null) return "";
    int cut = sid.LastIndexOf('-');
    if (cut < 0 || cut + 1 >= sid.Length) return "";
    uint rid;
    if (!uint.TryParse(sid.Substring(cut + 1), out rid)) return "";
    if (rid >= 0x5000) return "protected";
    if (rid >= 0x4000) return "system";
    if (rid >= 0x3000) return "high";
    if (rid >= 0x2000) return "medium";
    if (rid >= 0x1000) return "low";
    return "untrusted";
  }
}
"@
[CuToken]::Read()
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_is_unsupported() {
        let error = tree(&crate::tree::TreeQuery::default()).unwrap_err();
        assert_eq!(error.code(), "unsupported");
        assert!(error.to_string().contains("windows"));
    }

    #[test]
    fn element_action_is_unsupported() {
        use crate::backend::DesktopBackend;
        use crate::element::{ElementOp, ElementRequest};
        let error = WindowsDesktopBackend
            .element_action(&ElementRequest {
                op: ElementOp::Focus,
                index: 0,
                require_visible: false,
                app: None,
                window: None,
                session: None,
                tier: None,
                allow_foreground: false,
                require_background: false,
                no_overlay: true,
            })
            .unwrap_err();
        assert_eq!(error.error.code(), "unsupported");
        assert!(error.error.to_string().contains("windows"));
    }

    #[test]
    fn frontmost_parser_reads_two_lines() {
        let info = split_frontmost(b"notepad\nUntitled - Notepad\n");
        assert_eq!(info.app.as_deref(), Some("notepad"));
        assert_eq!(info.window.as_deref(), Some("Untitled - Notepad"));
    }

    #[test]
    fn doctor_uses_the_shared_shape() {
        let report = doctor(DoctorOptions::default()).unwrap();
        let value = serde_json::to_value(&report).unwrap();
        assert!(value["permissions_ok"].is_boolean());
        assert!(value["missing"].is_array());
        assert!(value["accessibility"].is_null());
        assert!(value["screen_capture"].is_null());
        assert!(value["screen_recording"].is_null());
        assert!(value["post_events"].is_null());
        assert!(value["input_monitoring"].is_null());
        assert!(value["input_monitor"].is_null());
        assert!(value["fix"].is_array());
        assert!(value["text"].is_string());
        assert!(value["identity"].is_object());
        assert!(value["session"].is_null());
        assert!(value["atspi_bus"].is_null());
        assert!(value["uiaccess"].is_boolean() || value["uiaccess"].is_null());
        assert!(value["integrity"].is_string() || value["integrity"].is_null());
        let note = value["note"].as_str().unwrap();
        assert!(note.contains("P12"), "{note}");
        assert!(report.permissions_ok);
        assert!(report.missing.is_empty());
    }
}

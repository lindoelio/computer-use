//! Windows frontmost-window lookup.
//!
//! Input uses enigo (Win32 `SendInput`). The full UI Automation tree is not
//! collected. The foreground window title and process name come from a
//! short-lived PowerShell call and are best-effort.

use std::time::Duration;

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

pub(crate) fn quit(_app: &str) -> Result<QuitResult, ComputerError> {
    Err(unsupported("quit"))
}

pub(crate) fn doctor(request: bool) -> Result<DoctorReport, ComputerError> {
    let mut report = DoctorReport::pending("P12");
    if request {
        report.note = Some(
            "accessibility and screen-capture probes land in P12. --request has no effect on this platform"
                .into(),
        );
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmost_parser_reads_two_lines() {
        let info = split_frontmost(b"notepad\nUntitled - Notepad\n");
        assert_eq!(info.app.as_deref(), Some("notepad"));
        assert_eq!(info.window.as_deref(), Some("Untitled - Notepad"));
    }
}

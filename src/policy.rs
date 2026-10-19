//! Built-in application deny list.
//!
//! P1 matches bundle ids, canonical executable paths, and a few display names.
//! A missing policy file does not deny ordinary apps. TOML allow and deny
//! lists arrive in P6. Admin policy cannot allow an app this list refuses.

use std::path::Path;

use crate::error::ComputerError;
use crate::target::AppSelector;

const SYSTEM_SETTINGS_BUNDLE: &str = "com.apple.systempreferences";

/// Bundle ids that are always refused. Comparison is case-insensitive.
const DENIED_BUNDLE_IDS: &[&str] = &[
    "com.apple.Terminal",
    "com.googlecode.iterm2",
    "dev.warp.Warp-Stable",
    "org.alacritty",
    "net.kovidgoyal.kitty",
    "com.mitchellh.ghostty",
    "com.github.wez.wezterm",
    "com.1password.1password",
    "com.agilebits.onepassword7",
    "com.bitwarden.desktop",
    "org.keepassxc.KeePassXC",
    "com.dashlane.Dashlane",
    "in.sinew.Enpass-Desktop",
    "com.apple.keychainaccess",
    "Microsoft.WindowsTerminal",
];

/// Executable file names, matched case-insensitively after canonicalization.
const DENIED_EXE_NAMES: &[&str] = &[
    "Terminal",
    "iTerm2",
    "Warp",
    "Alacritty",
    "kitty",
    "ghostty",
    "wezterm",
    "wezterm-gui",
    "powershell.exe",
    "cmd.exe",
    "wt.exe",
    "WindowsTerminal.exe",
    "gnome-terminal",
    "konsole",
    "xterm",
    "1Password",
    "Bitwarden",
    "KeePassXC",
    "Dashlane",
    "Enpass",
    "Keychain Access",
];

/// `.app` bundle directory names, matched as a single path component.
const DENIED_APP_BUNDLES: &[&str] = &[
    "Terminal.app",
    "iTerm.app",
    "iTerm2.app",
    "Warp.app",
    "Alacritty.app",
    "kitty.app",
    "Ghostty.app",
    "WezTerm.app",
    "1Password.app",
    "1Password 7.app",
    "Bitwarden.app",
    "KeePassXC.app",
    "Dashlane.app",
    "Enpass.app",
    "Keychain Access.app",
];

/// Display names refused before a bundle is resolved. `Terminal` is enough
/// to refuse `launch Terminal` without scanning the disk.
const DENIED_DISPLAY_NAMES: &[&str] = &[
    "Terminal",
    "iTerm",
    "iTerm2",
    "Warp",
    "Alacritty",
    "kitty",
    "Ghostty",
    "WezTerm",
    "1Password",
    "1Password 7",
    "Bitwarden",
    "KeePassXC",
    "Dashlane",
    "Enpass",
    "Keychain Access",
    "Windows Terminal",
];

/// Why a built-in rule refused an app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denial {
    /// `bundle`, `path`, `name`, or `system-settings`.
    pub source: &'static str,
    /// Human-readable explanation. Does not include typed text or URLs.
    pub message: String,
}

/// Refuse `selector` before it is resolved, when the selector itself is enough.
pub fn selector_denial(selector: &AppSelector) -> Option<Denial> {
    match selector {
        AppSelector::Pid(_) => None,
        AppSelector::BundleId(bundle_id) => bundle_denial(bundle_id).map(|message| Denial {
            source: "bundle",
            message,
        }),
        AppSelector::Name(name) => {
            if denied_display_name(name) {
                Some(Denial {
                    source: "name",
                    message: format!("built-in deny list refuses {name}"),
                })
            } else {
                None
            }
        }
        AppSelector::Path(path) => path_denial(path).map(|message| Denial {
            source: "path",
            message,
        }),
    }
}

/// Refuse a resolved app. System Settings is refused only when the targeted
/// window title or accessibility identifier mentions Privacy or Security.
pub fn identity_denial(
    bundle_id: Option<&str>,
    exe_path: Option<&Path>,
    window_title: Option<&str>,
    ax_identifier: Option<&str>,
) -> Option<Denial> {
    if let Some(bundle_id) = bundle_id {
        if let Some(message) = bundle_denial(bundle_id) {
            return Some(Denial {
                source: "bundle",
                message,
            });
        }
        if bundle_id.eq_ignore_ascii_case(SYSTEM_SETTINGS_BUNDLE)
            && privacy_or_security(window_title, ax_identifier)
        {
            return Some(Denial {
                source: "system-settings",
                message: "built-in deny list refuses System Settings while the window title or identifier contains Privacy or Security".into(),
            });
        }
    }
    if let Some(path) = exe_path {
        if let Some(message) = path_denial(path) {
            return Some(Denial {
                source: "path",
                message,
            });
        }
    }
    None
}

/// Map a denial to [`ComputerError::PolicyDenied`].
pub fn deny_or_allow(denial: Option<Denial>) -> Result<(), ComputerError> {
    match denial {
        None => Ok(()),
        Some(denial) => Err(ComputerError::PolicyDenied(denial.message)),
    }
}

/// `loginwindow`, `WindowServer`, and this process cannot be quit.
pub fn quit_is_protected(
    pid: i32,
    current_pid: i32,
    bundle_id: Option<&str>,
    name: &str,
    path: Option<&str>,
) -> bool {
    if pid == current_pid {
        return true;
    }
    if bundle_id.is_some_and(|id| id.eq_ignore_ascii_case("com.apple.loginwindow")) {
        return true;
    }
    let name = name.trim();
    if name.eq_ignore_ascii_case("loginwindow") || name.eq_ignore_ascii_case("WindowServer") {
        return true;
    }
    path.is_some_and(|path| {
        Path::new(path)
            .file_name()
            .and_then(|file| file.to_str())
            .is_some_and(|file| {
                file.eq_ignore_ascii_case("loginwindow")
                    || file.eq_ignore_ascii_case("WindowServer")
            })
    })
}

fn bundle_denial(bundle_id: &str) -> Option<String> {
    DENIED_BUNDLE_IDS
        .iter()
        .find(|id| id.eq_ignore_ascii_case(bundle_id))
        .map(|id| format!("built-in deny list refuses {id}"))
}

fn path_denial(path: &Path) -> Option<String> {
    if let Some(file) = path.file_name().and_then(|file| file.to_str()) {
        if DENIED_EXE_NAMES
            .iter()
            .any(|name| name.eq_ignore_ascii_case(file))
        {
            return Some(format!(
                "built-in deny list refuses executable {}",
                path.display()
            ));
        }
    }
    for component in path.components() {
        let text = component.as_os_str().to_string_lossy();
        if DENIED_APP_BUNDLES
            .iter()
            .any(|name| text.eq_ignore_ascii_case(name))
        {
            return Some(format!(
                "built-in deny list refuses path {}",
                path.display()
            ));
        }
    }
    None
}

fn denied_display_name(name: &str) -> bool {
    DENIED_DISPLAY_NAMES
        .iter()
        .any(|denied| denied.eq_ignore_ascii_case(name.trim()))
}

fn privacy_or_security(window_title: Option<&str>, ax_identifier: Option<&str>) -> bool {
    [window_title, ax_identifier]
        .into_iter()
        .flatten()
        .any(|text| {
            let lower = text.to_ascii_lowercase();
            lower.contains("privacy") || lower.contains("security")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn terminal_bundle_id_and_fixture_path_are_denied() {
        let by_id = identity_denial(Some("com.apple.Terminal"), None, None, None).unwrap();
        assert_eq!(by_id.source, "bundle");
        assert!(by_id.message.contains("com.apple.Terminal"));

        let path = Path::new("/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal");
        let by_path = identity_denial(None, Some(path), None, None).unwrap();
        assert_eq!(by_path.source, "path");
        assert!(by_path.message.contains("Terminal.app"));

        let selector = AppSelector::BundleId("com.apple.Terminal".into());
        assert!(selector_denial(&selector).is_some());
        let named = AppSelector::Name(r#"Foo "Bar""#.into());
        assert!(selector_denial(&named).is_none());
    }

    #[test]
    fn every_listed_bundle_and_exe_is_denied_and_notes_is_not() {
        for bundle_id in DENIED_BUNDLE_IDS {
            assert!(
                identity_denial(Some(bundle_id), None, None, None).is_some(),
                "{bundle_id}"
            );
        }
        for exe in DENIED_EXE_NAMES {
            let path = PathBuf::from("/usr/bin").join(exe);
            assert!(
                identity_denial(None, Some(&path), None, None).is_some(),
                "{exe}"
            );
        }
        assert!(identity_denial(
            Some("com.apple.Notes"),
            Some(Path::new(
                "/System/Applications/Notes.app/Contents/MacOS/Notes"
            )),
            None,
            None
        )
        .is_none());
        assert!(identity_denial(
            None,
            Some(Path::new("/usr/local/bin/TerminalHelper")),
            None,
            None
        )
        .is_none());
    }

    #[test]
    fn system_settings_privacy_window_is_denied_and_other_windows_are_not() {
        assert!(identity_denial(
            Some(SYSTEM_SETTINGS_BUNDLE),
            Some(Path::new(
                "/System/Applications/System Settings.app/Contents/MacOS/System Settings"
            )),
            Some("Privacy & Security"),
            None,
        )
        .is_some());
        assert!(identity_denial(
            Some(SYSTEM_SETTINGS_BUNDLE),
            None,
            Some("Displays"),
            Some("AXIdentifier"),
        )
        .is_none());
        assert!(identity_denial(Some(SYSTEM_SETTINGS_BUNDLE), None, None, None).is_none());
        assert!(identity_denial(
            Some(SYSTEM_SETTINGS_BUNDLE),
            None,
            None,
            Some("Privacy_Security"),
        )
        .is_some());
    }

    #[test]
    fn powershell_cmd_and_wt_paths_are_denied() {
        for exe in ["powershell.exe", "cmd.exe", "wt.exe", "gnome-terminal"] {
            let path = PathBuf::from(r"C:\Windows\System32").join(exe);
            assert!(path_denial(&path).is_some(), "{exe}");
        }
    }

    #[test]
    fn quit_protects_loginwindow_windowserver_and_self() {
        assert!(quit_is_protected(9, 9, None, "computer-use", None));
        assert!(quit_is_protected(
            2,
            9,
            Some("com.apple.loginwindow"),
            "loginwindow",
            None
        ));
        assert!(quit_is_protected(3, 9, None, "WindowServer", None));
        assert!(!quit_is_protected(
            10,
            9,
            Some("com.apple.Notes"),
            "Notes",
            Some("/System/Applications/Notes.app/Contents/MacOS/Notes")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_a_terminal_executable_is_denied() {
        let dir = std::env::temp_dir().join(format!(
            "computer-use-policy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let real = dir.join("Terminal");
        fs::write(&real, b"fixture").unwrap();
        let link = dir.join("alias");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let canonical = fs::canonicalize(&link).unwrap();
        assert_eq!(
            canonical.file_name().and_then(|name| name.to_str()),
            Some("Terminal")
        );
        assert!(identity_denial(None, Some(&canonical), None, None).is_some());
        let _ = fs::remove_dir_all(&dir);
    }
}

# Packaging Computer Use.app

`Computer Use.app` gives the `computer-use` binary a stable TCC identity. macOS attaches Accessibility, Screen Recording, Input Monitoring, and Post Event to a bundle identifier, not to whatever path you happened to compile. The identifier is `ai.lindoelio.computer-use`.

The bundle is an agent app (`LSUIElement` is true), so it has no Dock icon. It does not grant those permissions. You still turn the switch on in System Settings. The usage strings in `Info.plist` only name the prompt.

## Local build

From the repository root, after `cargo build --release`:

```bash
scripts/package-macos-app.sh target/release/computer-use dist
```

The first argument is the binary. It defaults to `target/release/computer-use`, or to `COMPUTER_USE_BINARY`. Pass an arm64 or universal binary the same way. The second argument is the output directory (`COMPUTER_USE_OUTPUT_DIR`, default `dist`).

With no `MACOS_SIGN_IDENTITY`, the script ad-hoc signs the bundle (`codesign --force --sign -`). That is enough for TCC on the Mac that built it, but the grant follows the ad-hoc code requirement: replacing the binary on update can invalidate Accessibility, Screen Recording, Input Monitoring, and Post Event, and the user must grant them again. Configure `MACOS_SIGN_IDENTITY`, `APPLE_ID`, `APPLE_APP_SPECIFIC_PASSWORD`, and `APPLE_TEAM_ID` for a Developer ID signature plus notarization, which keeps the grant stable across updates. The ad-hoc zip is `computer-use-macos-arm64-app-unsigned.zip`. The script prints the zip path.

Check the bundle:

```bash
codesign --verify --verbose=2 "dist/Computer Use.app"
plutil -lint "dist/Computer Use.app/Contents/Info.plist"
plutil -lint packaging/macos/Info.plist
plutil -lint packaging/macos/entitlements.plist
"dist/Computer Use.app/Contents/MacOS/computer-use" doctor
```

`doctor` should report `bundle_id` `ai.lindoelio.computer-use` and `identity.bundled` true. A normal `doctor` does not prompt and does not open Settings. The process exits 0 when the report is produced. `permissions_ok` is false when a required permission is missing. The response field `ok` means the report was produced, not that every permission is granted. The readable summary is the JSON field `text`. The one-shot `doctor` command also writes that summary to stderr. `exec` and `serve` do not.

## CLI shim

TCC follows the bundle only when you launch the executable inside `Computer Use.app`. A binary copied out of the bundle, or `target/release/computer-use`, is a bare executable. Its bundle id is null, and TCC attributes the process to the terminal that started it (Terminal, iTerm, Grok, or whatever `doctor` prints as `responsible`).

Keep a shim on `PATH` that points at the bundled executable:

```bash
mkdir -p ~/.local/bin
ln -sfn "/Applications/Computer Use.app/Contents/MacOS/computer-use" ~/.local/bin/computer-use
```

Use the path you actually packaged if the app is not in `/Applications`. `doctor` prints this recommendation when `identity.bundled` is false.

## Developer ID signature

Create a Developer ID Application certificate, then:

```bash
codesign --deep --force --options runtime --timestamp \
  --entitlements packaging/macos/entitlements.plist \
  --sign "Developer ID Application: Your Name (TEAMID)" \
  "dist/Computer Use.app"
```

`scripts/package-macos-app.sh` runs that command when `MACOS_SIGN_IDENTITY` is set to the identity name. The zip is then `computer-use-macos-arm64-app.zip`.

Hardened runtime is `--options runtime`. The timestamp needs Apple's timestamp server.

## Notarization

After the Developer ID signature:

```bash
ditto -c -k --keepParent "dist/Computer Use.app" dist/computer-use-notarize.zip
xcrun notarytool submit dist/computer-use-notarize.zip \
  --apple-id "you@example.com" \
  --password "app-specific-password" \
  --team-id "TEAMID" \
  --wait
xcrun stapler staple "dist/Computer Use.app"
```

The script does this when `MACOS_SIGN_IDENTITY`, `APPLE_ID`, `APPLE_APP_SPECIFIC_PASSWORD`, and `APPLE_TEAM_ID` are all set. It staples the app and zips again so the ticket is inside the artifact. It does not notarize an ad-hoc bundle.

## GitHub secrets

The release workflow signs only when the first three secrets exist. It notarizes only when the Apple ID secrets exist too. With none of them, it uploads `computer-use-macos-arm64-app-unsigned.zip` and adds a line to the release notes saying the zip is ad-hoc signed and not notarized.

| Secret | Required for | Value |
| --- | --- | --- |
| `MACOS_CERTIFICATE_P12` | signing | base64 of the Developer ID Application `.p12` |
| `MACOS_CERTIFICATE_PASSWORD` | signing | password for that `.p12` |
| `APPLE_TEAM_ID` | signing | Apple team id, such as `ABCDE12345` |
| `APPLE_ID` | notarization | Apple ID email |
| `APPLE_APP_SPECIFIC_PASSWORD` | notarization | app-specific password for `notarytool` |

The workflow imports the certificate into a temporary keychain, sets the key partition list so `codesign` can use it without a UI, derives the Developer ID Application identity, and deletes the keychain at the end of the job.

## Entitlements

`packaging/macos/entitlements.plist` is an empty dictionary. Hardened runtime does not need an extra key for this binary.

| Entitlement | Set | Why |
| --- | --- | --- |
| `com.apple.security.app-sandbox` | no | This is not an App Store app. The sandbox blocks the accessibility and event APIs the tool uses. |
| `com.apple.security.automation.apple-events` | no | Apple Events go through the separate `osascript` process. |
| `com.apple.security.cs.disable-library-validation` | no | The default binary does not load third-party libraries. SkyLight, when the `skylight` feature is compiled in, is an Apple-signed system framework. |
| `com.apple.security.cs.allow-jit` | no | The binary does not JIT. |
| `com.apple.security.cs.allow-unsigned-executable-memory` | no | The binary does not allocate unsigned executable memory. |
| `com.apple.security.cs.allow-dyld-environment-variables` | no | Not used. |
| `com.apple.security.device.audio-input` | no | The tool does not capture the microphone. |

TCC still gates Accessibility, Screen Recording, Input Monitoring, and Post Event. An entitlement cannot turn those on.

## Granting permissions

Run `computer-use doctor`. For each entry in `missing`, enable the `target` in the pane named by `fix`. The URLs are:

| Permission | Pane | URL |
| --- | --- | --- |
| Accessibility | Accessibility | `x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility` |
| Screen Recording | Screen Recording | `x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture` |
| Post Event | Accessibility | `x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility` |
| Input Monitoring | Input Monitoring | `x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent` |

Post Event uses the Accessibility pane. Apple maps that TCC service and the Accessibility service to the same list.

When `identity.bundled` is true, the target is **Computer Use**. When you launched a bare binary from a terminal, the target is that terminal app (`identity.responsible`). `doctor --open-settings` opens the first missing URL with `open`. `doctor --prompt` (alias `--request`) asks macOS to show the dialogs. Leave both off for an agent. The dialogs are asynchronous: the report can still say the permission is missing until the user decides and you run `doctor` again.

`input_monitoring` is the Input Monitoring preflight. `input_monitor` is whether this process created the listen-only event tap. It stays null until a policy with `pause_on_user_input` tries to install the tap.

## Limitations

- An ad-hoc signature is local. Gatekeeper on another Mac will not accept the unsigned zip. Re-sign and notarize before you distribute it.
- The usage strings do not grant TCC. The user still toggles the switch.
- `CFBundleVersion` is the Cargo version with any pre-release suffix removed, and it must be one to three numeric components. `0.1.0` is valid.
- Linux `doctor` reports the X11 or Wayland session and whether an AT-SPI bus address is present. It does not connect to the bus. That connection is P13. Windows reports UIAccess and the integrity label when the token read works. Those are not required grants. The UIA probe is P12.
- Notarization in GitHub Actions runs only when the Apple ID secrets are configured. This repository does not ship a stapled ticket until those secrets exist.
- `packaging/macos/Info.plist` contains `@CARGO_VERSION@`. The script replaces it. Do not copy the template into an app by hand without that replacement.

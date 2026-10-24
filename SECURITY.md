# Security

`computer-use` can observe and control applications in the user's desktop session. Its policy checks are safeguards, not an operating-system sandbox or an authorization boundary against untrusted local code.

## Reporting a vulnerability

Do not publish exploit details, credentials, screenshots of private applications, or unredacted session data in a public issue.

Use private vulnerability reporting in the [repository's Security tab](https://github.com/lindoelio/computer-use/security) if it is available. Otherwise, request a private reporting channel from the maintainer, without posting the vulnerability details. This repository does not specify a dedicated security email address, response-time commitment, or supported-version maintenance window.

Include the affected version or commit, operating system, enabled Cargo features, minimal reproduction using disposable data, expected behavior, actual behavior, and security impact. For policy issues, include a sanitized policy and relevant flags or environment overrides.

## Operational boundaries

- Run only trusted automation requests. The CLI accepts commands, `exec` accepts JSON, and `serve` / `--json` consume newline-delimited JSON on stdin. There is no shipped HTTP, TCP, or MCP server interface. CDP connects to a browser endpoint; the desktop overlay uses a local Unix socket.
- Grant only the operating-system permissions needed for the task. `doctor` reports grants; a successful report (`ok: true`) does not mean all permissions are present. Normal `doctor` does not prompt; permission prompting is explicit.
- A display filter, background input tier, browser profile, or session namespace is not a sandbox. Use a separate desktop session or VM when a real isolation boundary is needed; see [docs/ISOLATION.md](docs/ISOLATION.md).
- Keep CDP endpoints private and use a dedicated browser profile for automation. Attaching to a browser grants access to its pages and session data; managed `browser launch` uses a separate profile, not a separate security principal.

## Policy and confirmation

The implementation is in `src/policy.rs` and the dispatch guards in `src/protocol.rs`. See [docs/policy.example.toml](docs/policy.example.toml) for configuration examples.

- Without a policy file, ordinary apps are allowed and the built-in deny list still applies. Absence of a file is not default deny.
- The default user file is `~/.config/computer-use/policy.toml`; `XDG_CONFIG_HOME` changes the base directory and `--policy` selects an explicit file. Admin policy defaults to `/etc/computer-use/policy.toml` on Unix or `%ProgramData%\computer-use\policy.toml` on Windows. `COMPUTER_USE_ADMIN_POLICY` can override the admin path, so environment and launch configuration must be trusted.
- Deny rules take precedence over allow rules; admin allow entries do not grant access. A malformed present admin policy fails closed with `policy_denied`. An explicitly selected missing user file is an error.
- The user file can explicitly exempt built-in entries through `[builtin_deny] exempt = [...]`. This weakens the built-in safeguards; ordinary allow entries are not equivalent to an exemption, and an exemption does not cancel configured deny rules.
- Sensitive actions, including quit, clipboard writes, Apple events, Shortcuts, and recognized destructive controls or system shortcuts, require a confirmation hook. With no hook they return `confirm_required`; nonzero exit or timeout returns `confirm_denied`. A hook's exit status 0 authorizes the action. Trust the hook executable and its configuration; do not configure an unconditional approval hook for untrusted requests.
- Explicit secure-element actions refuse protected fields. Text input without an explicit element uses a focused-element guard: `secure_focus` defaults to `deny`, while `unreadable_focus` defaults to `warn`. Configuring `warn` permits input with a warning; it is not refusal. Browser typing also rejects recognized password and one-time-code fields. These checks do not guarantee detection of every sensitive control.
- `pause_on_user_input` is off by default. On macOS it attempts to install a listen-only event tap. If the tap is unavailable, actions can still run with a warning; do not treat this mechanism as a guaranteed emergency stop.

## Data handling

- Preserve the audit schema's exclusion of typed text, element values, clipboard contents, blocked URLs, and verbatim script bodies. Apple-event metadata records script length and hash rather than the body. Do not introduce sensitive payloads into diagnostic logs or warnings.
- Audit logging is configurable and a write failure does not by itself stop an action. It is not a tamper-proof security record.
- Session snapshots redact recognized secure-field values, but ordinary tree text, screenshots, clipboard images, browser profiles, and command responses can contain private data. Do not assume screenshots are sanitized by text-tree redaction. File-based OCR does not apply window secure-field redaction.
- Session directories and cache files use restrictive Unix permissions (`0700` directories, `0600` files). Treat these artifacts as sensitive on every platform; do not commit them or attach them to reports without reviewing their contents.

## Development and distribution

Keep policy, confirmation, secure-field, and stale-element guards in the shared action path. Test refusal paths without posting real desktop input; see [TESTING.md](TESTING.md).

Keep the crate-root unsafe-code lint and document local FFI safety requirements; see [STYLEGUIDE.md](STYLEGUIDE.md#unsafe-code). Bound subprocess and framework operations with the existing timeout mechanisms rather than bypassing them.

Do not commit registry tokens, signing certificates, Apple credentials, or private browser profiles. Release credentials belong in the workflow's configured secrets. Verify downloaded release archives using the published SHA-256 files. macOS signing and notarization requirements, including the explicit unsigned-release override, are documented in [docs/PACKAGING.md](docs/PACKAGING.md).

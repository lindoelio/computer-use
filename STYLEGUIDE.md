# Style guide

Conventions used in this repository. `cargo fmt --all` and `cargo clippy --all-targets --all-features -- -D warnings` are the mechanical gates; this page covers what they do not decide.

## Toolchain and formatting

- Rust 2021 edition; minimum supported Rust version 1.91 (`package.rust-version` in `Cargo.toml`).
- rustfmt with default settings. There is no `rustfmt.toml`, so let the formatter own layout and do not hand-align code.
- Clippy must pass on `--all-targets --all-features` with `-D warnings` on all three operating systems. Fix the lint or justify an `#[allow]` with a comment.

## Naming

- Standard Rust naming: `snake_case` for functions, methods, modules, and files; `UpperCamelCase` for types and enum variants; `SCREAMING_SNAKE_CASE` for constants.
- Directory modules use `mod.rs` (`src/backend/macos/mod.rs`); leaf modules are `snake_case.rs`.
- Test names are full descriptive phrases in `snake_case`, for example `auto_plan_uses_ax_for_presses_and_event_then_activate_for_points`, so a failing line names the behavior that broke.
- JSON keys and the action tag are `snake_case`; the action discriminator is `action`.
- Keep error message text lowercase and specific, naming the offending flag, field, or value.

## Documentation comments

- Every module starts with a `//!` header that states what the module owns.
- Public items carry `///` doc comments. Document contracts, units, defaults, and limits rather than restating the signature.
- Behavior that matters to callers (platform differences, defaults, caps) belongs in the doc comment as well as in `README.md` or `docs/CLI.md`.

## Comments

- Add code comments only when they are highly necessary to explain non-obvious intent, workarounds, or critical constraints. Do not narrate what the code does.
- Every `unsafe` block gets a `// SAFETY:` comment naming the pointer lifetime and the framework precondition.

## Unsafe code

- `src/lib.rs` and `src/main.rs` use `#![deny(unsafe_code)]`.
- Keep exceptions local to the FFI module, function, or test helper that needs them. Existing examples include backend modules, OCR functions, and cache environment guards; do not relax the crate-root lint.
- Prefer safe wrappers and bindings. `uiautomation`, `atspi`, `ashpd`, `x11rb`, `arboard`, and `chromiumoxide` are called through their safe APIs.

## Error handling

- Backend and validation failures use `Result<_, ComputerError>`; the dispatcher returns `Response`. Reuse an appropriate `ComputerError` variant rather than inventing an error-code string. `ComputerError::code()` is the stable JSON contract; add a variant only for a genuinely new error category.
- Bad input is `invalid_arguments`; off-platform or feature-off is `unsupported`; a framework failure that is not in the documented table is `execution_failed`.
- Do not panic on user input. `dispatch_with` catches panics and reports `execution_failed` as a last resort, but that is recovery, not a design.
- Messages should tell the caller what to do next where possible (for example, naming the Settings pane, the missing flag, or the required feature).

## Serde and the protocol

- The action enum uses `#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]`. Unknown fields are rejected so a typo cannot silently fall back to a default.
- Optional fields use `#[serde(default, skip_serializing_if = "Option::is_none")]`; booleans that default to false use the shared `is_false` helper.
- Global request keys (`app`, `window`, `element`, `tier`, `session`, and the rest of the shared context) are siblings of `action` and are stripped before the action decodes. New shared keys must be added to the strip list in `src/protocol.rs`, the CLI, and `docs/CLI.md`.
- Coordinates are logical screen points in the native protocol; image-pixel coordinates exist only behind `coord: "image"` and the adapters.

## Module and API surface

- `src/lib.rs` is the public surface: re-export what callers need and keep implementation modules private where possible (`mod`, not `pub mod`).
- Test scaffolding that is compiled into test builds only lives behind `#[cfg(test)]` (`src/test_backend.rs`).
- Keep pure logic platform-independent so unit tests can cover it (tree rendering, coordinate remapping, policy resolution, adapter translation). Platform calls stay in the backend module that owns them.

## Platform code

- Gate platform code with `#[cfg(target_os = "...")]` and target-gated dependencies so native code builds only on the relevant platform. Cargo may still resolve or fetch dependencies for other targets.
- Every command exists in the clap tree on every OS. Off-platform execution returns `unsupported` with the OS name in the message; do not hide the command.
- Use `ComputerError::Unsupported` for unavailable backend operations, matching the trait's unsupported defaults. Other defaults delegate to shared implementations; do not assume every default is a no-op.
- Feature-gated behavior returns `unsupported` when the feature is off; do not silently fall back to a different mechanism.

## Sensitive data

Follow [SECURITY.md](SECURITY.md#data-handling) for logging and redaction constraints.

## Tests

- Unit tests live in a `#[cfg(test)] mod tests` block at the bottom of the module they cover.

See [TESTING.md](TESTING.md) for desktop-safety rules, environment isolation, and test commands.

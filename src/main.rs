//! `computer-use` command line interface.
//!
//! Every action prints one JSON object on stdout. `--help` and `--version`
//! stay human-readable. `--json` and `serve` read newline-delimited requests.

#![deny(unsafe_code)]

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use computer_use::{
    handle_request_line, parse_region, Action, CallContext, ImageFormat, MouseButton, Response,
    ScrollDirection,
};

/// Cross-platform desktop automation for agents.
///
/// Every action prints one JSON object on stdout:
/// `{"ok":true,...}` or `{"ok":false,"error":{"code":"...","message":"..."}}`.
/// Failed actions exit non-zero.
///
/// `computer-use serve` and `computer-use --json` read newline-delimited JSON
/// requests from stdin and write one JSON response per line. `exec` runs one
/// JSON action. Serve mode always prints compact JSON so each response stays
/// on a single line.
#[derive(Debug, Parser)]
#[command(name = "computer-use", version, propagate_version = true)]
struct Cli {
    /// Pretty-print one-shot JSON. Serve mode stays one object per line.
    #[arg(long, global = true)]
    pretty: bool,

    /// Read newline-delimited JSON actions from stdin and write one JSON response per line.
    #[arg(long)]
    json: bool,

    /// Target application: localized name, bundle id, path, or pid.
    #[arg(long, global = true, value_name = "NAME|BUNDLE|PID")]
    app: Option<String>,

    /// Target window: title, z-order index (0 is front), or `id:<number>`.
    #[arg(long, global = true, value_name = "TITLE|INDEX|ID")]
    window: Option<String>,

    /// Element index from the latest `tree` for this app and window.
    #[arg(long, global = true, value_name = "N")]
    element: Option<u32>,

    /// Fail when the element is outside the window and scroll-into-view cannot run.
    #[arg(long, global = true)]
    require_visible: bool,

    /// Print a diff against the cached tree. This is the default once a baseline exists.
    #[arg(long, global = true, conflicts_with = "full")]
    diff: bool,

    /// Print the full tree and replace the cached baseline.
    #[arg(long, global = true)]
    full: bool,

    /// Session cache namespace. Defaults to this process id.
    #[arg(long, global = true, value_name = "ID")]
    session: Option<String>,

    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Debug, Subcommand)]
enum SessionCommand {
    /// Restore Electron accessibility flags this process changed and stop observers.
    End,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// Capture the screen, a region, or one window, and write a PNG or JPEG file.
    ///
    /// `--app` and `--window` capture that window. Without them, the capture is
    /// the full desktop or `--region`. The default max width is 1440.
    /// `--max-width 0` keeps the source width. `--canvas 1440x900` letterboxes.
    ///
    /// Example: computer-use screenshot --app Notes --window 0 --max-width 1440
    Screenshot {
        /// File to write. Defaults to a path in the system temp directory.
        #[arg(long, visible_alias = "path", value_name = "PATH")]
        output: Option<PathBuf>,

        /// Logical region `x,y,width,height` in the same space as click coordinates.
        #[arg(long, value_name = "X,Y,W,H")]
        region: Option<String>,

        /// Image format. Default: png.
        #[arg(long, value_enum)]
        format: Option<CliFormat>,

        /// Scale the image down so its width is at most N pixels. Default: 1440. 0 keeps the source width.
        #[arg(long, value_name = "N")]
        max_width: Option<u32>,

        /// Letterbox into WxH pixels, for example 1440x900. The image is not scaled up.
        #[arg(long, value_name = "WxH")]
        canvas: Option<String>,

        /// Add a standard base64 `data` field to the JSON result.
        #[arg(long)]
        base64: bool,
    },

    /// Move the pointer to a logical point and click, or press an element.
    ///
    /// `--element N` is an alias of `press`. `--x` and `--y` click that point.
    /// Pass one of those forms, not both.
    ///
    /// Example: computer-use click --x 120 --y 240 --button right --double
    Click {
        /// Logical x coordinate. Required with `--y` when `--element` is omitted.
        #[arg(long)]
        x: Option<i32>,
        /// Logical y coordinate. Required with `--x` when `--element` is omitted.
        #[arg(long)]
        y: Option<i32>,
        /// Mouse button. Default: left.
        #[arg(long, value_enum, default_value = "left")]
        button: CliButton,
        /// Click twice. With `--element`, this presses twice.
        #[arg(long)]
        double: bool,
    },

    /// Press an element from the current tree. `--count` may be 1, 2, or 3.
    ///
    /// Example: computer-use press --app Notes --element 12 --count 2
    Press {
        /// How many times to press. Default: 1.
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=3))]
        count: u8,
    },

    /// Set an element's value. Secure fields are refused.
    ///
    /// Example: computer-use set-value --element 4 --value "hello"
    #[command(name = "set-value", visible_alias = "set_value")]
    SetValue {
        /// New value.
        #[arg(long)]
        value: String,
    },

    /// Select a character range. Secure fields are refused.
    ///
    /// Example: computer-use select-text --element 4 --start 0 --end 5
    #[command(name = "select-text", visible_alias = "select_text")]
    SelectText {
        /// First selected character.
        #[arg(long)]
        start: i64,
        /// Character after the selection.
        #[arg(long)]
        end: i64,
    },

    /// Perform an allowlisted accessibility action.
    ///
    /// Example: computer-use perform-action --element 9 --name AXShowMenu
    #[command(name = "perform-action", visible_alias = "perform_action")]
    PerformAction {
        /// Action name, such as `AXShowMenu` or `AXPress`.
        #[arg(long)]
        name: String,
    },

    /// Scroll an element into view.
    ///
    /// Example: computer-use scroll-to --element 9
    #[command(
        name = "scroll-to",
        visible_alias = "scroll-to-visible",
        aliases = ["scroll_to", "scroll_to_visible"]
    )]
    ScrollTo,

    /// Focus an element.
    ///
    /// Example: computer-use focus --element 4
    Focus,

    /// Move the pointer without clicking.
    ///
    /// Example: computer-use move --x 400 --y 300
    Move {
        /// Logical x coordinate.
        #[arg(long)]
        x: i32,
        /// Logical y coordinate.
        #[arg(long)]
        y: i32,
    },

    /// Type Unicode text into the focused control.
    ///
    /// Example: computer-use type --text "hello"
    Type {
        /// Text to type.
        #[arg(long)]
        text: String,
    },

    /// Press a key combination.
    ///
    /// Modifiers: cmd, command, meta, super, ctrl, control, alt, option, shift.
    /// Keys: enter, tab, esc, space, arrows, f1-f12, letters, digits, and
    /// backspace, delete, home, end, pageup, pagedown.
    ///
    /// Example: computer-use key --keys "cmd+shift+4"
    Key {
        /// Combination such as `cmd+c` or `ctrl+shift+tab`.
        #[arg(long, value_name = "COMBO")]
        keys: String,
    },

    /// Scroll the mouse wheel.
    ///
    /// Use `--dx` and `--dy`, or `--direction` with optional `--amount`.
    /// Positive dy scrolls down. Positive dx scrolls right.
    /// Optional `--x` and `--y` move the pointer first and must be passed together.
    ///
    /// Example: computer-use scroll --direction down --amount 3
    Scroll {
        /// Move the pointer to this logical x before scrolling.
        #[arg(long)]
        x: Option<i32>,
        /// Move the pointer to this logical y before scrolling.
        #[arg(long)]
        y: Option<i32>,
        /// Horizontal wheel steps. Positive is right.
        #[arg(long)]
        dx: Option<i32>,
        /// Vertical wheel steps. Positive is down.
        #[arg(long)]
        dy: Option<i32>,
        /// Scroll direction used with `--amount`.
        #[arg(long, value_enum)]
        direction: Option<CliDirection>,
        /// Wheel steps used with `--direction`. Default: 1.
        #[arg(long)]
        amount: Option<i32>,
    },

    /// Report displays, cursor, frontmost window, and a best-effort accessibility tree.
    ///
    /// The command does not panic when a subsystem is unavailable. Missing data
    /// is null and `note` explains why.
    ///
    /// Example: computer-use get-state
    #[command(name = "get-state", visible_alias = "get_state")]
    GetState,

    /// Sleep, then return `waited_ms`.
    ///
    /// Pass `--ms`, `--seconds`, or both. The durations are added.
    ///
    /// Example: computer-use wait --ms 250
    Wait {
        /// Milliseconds to sleep.
        #[arg(long, value_name = "N")]
        ms: Option<u64>,
        /// Seconds to sleep. Fractions are allowed.
        #[arg(long, value_name = "N")]
        seconds: Option<f64>,
    },

    /// List connected displays without capturing pixels.
    ///
    /// Example: computer-use displays
    Displays,

    /// List running applications.
    ///
    /// Example: computer-use list-apps --all
    #[command(name = "list-apps", visible_alias = "list_apps")]
    ListApps {
        /// Include accessory and prohibited apps.
        #[arg(long)]
        all: bool,
    },

    /// List on-screen windows in z-order. Index 0 is front.
    ///
    /// `--app` keeps that application's windows and renumbers z-order.
    /// `ax_matched` is true when an accessibility window shares that row's bounds.
    ///
    /// Example: computer-use list-windows --app Finder
    #[command(name = "list-windows", visible_alias = "list_windows")]
    ListWindows,

    /// Launch an application by name, bundle id, path, or pid.
    ///
    /// The positional name wins over `--app`. The command refuses the built-in
    /// deny list before it asks the system to open anything.
    ///
    /// Example: computer-use launch com.apple.TextEdit
    Launch {
        /// Application name, bundle id, path, or pid.
        #[arg(value_name = "APP")]
        app: Option<String>,
    },

    /// Bring an application forward.
    ///
    /// Requires `--app`. `--window` selects a window and is checked before activation.
    ///
    /// Example: computer-use activate --app Notes --window 0
    Activate,

    /// Ask an application to quit. This does not force-terminate.
    ///
    /// Requires `--app`. loginwindow, WindowServer, and this process are refused.
    ///
    /// Example: computer-use quit --app Notes
    Quit,

    /// Report Accessibility and Screen Recording permission.
    ///
    /// A normal call does not prompt. `--request` asks for both permissions.
    ///
    /// Example: computer-use doctor
    Doctor {
        /// Prompt for Accessibility and request Screen Recording.
        #[arg(long)]
        request: bool,
    },

    /// Print the key window's accessibility tree.
    ///
    /// This does not click. `--diff` is the default once a baseline exists.
    /// `--full` prints the whole tree and replaces that baseline.
    ///
    /// Example: computer-use tree --app Notes --window 0
    Tree,

    /// Session cache and accessibility teardown.
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },

    /// Read newline-delimited JSON actions from stdin and write one JSON response per line.
    ///
    /// Example: printf '%s\n' '{"id":1,"action":"wait","ms":10}' | computer-use serve
    Serve,

    /// Run one JSON action from an argument.
    ///
    /// Example: computer-use exec '{"action":"wait","ms":10}'
    Exec {
        /// JSON object with an `action` field and optional `id`.
        json: String,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliFormat {
    /// Portable Network Graphics.
    #[value(name = "png")]
    Png,
    /// JPEG. `jpg` is accepted as an alias.
    #[value(name = "jpeg", alias = "jpg")]
    Jpeg,
}

impl From<CliFormat> for ImageFormat {
    fn from(value: CliFormat) -> Self {
        match value {
            CliFormat::Png => Self::Png,
            CliFormat::Jpeg => Self::Jpeg,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliButton {
    #[value(name = "left")]
    Left,
    #[value(name = "right")]
    Right,
    #[value(name = "middle")]
    Middle,
}

impl From<CliButton> for MouseButton {
    fn from(value: CliButton) -> Self {
        match value {
            CliButton::Left => Self::Left,
            CliButton::Right => Self::Right,
            CliButton::Middle => Self::Middle,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliDirection {
    #[value(name = "up")]
    Up,
    #[value(name = "down")]
    Down,
    #[value(name = "left")]
    Left,
    #[value(name = "right")]
    Right,
}

impl From<CliDirection> for ScrollDirection {
    fn from(value: CliDirection) -> Self {
        match value {
            CliDirection::Up => Self::Up,
            CliDirection::Down => Self::Down,
            CliDirection::Left => Self::Left,
            CliDirection::Right => Self::Right,
        }
    }
}

fn main() -> ExitCode {
    let code = run();
    computer_use::shutdown_sessions();
    ExitCode::from(code)
}

fn run() -> u8 {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => match error.kind() {
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => error.exit(),
            _ => {
                emit(
                    &Response::failure("invalid_arguments", clap_message(&error)),
                    false,
                );
                return 2;
            }
        },
    };

    match (&cli.command, cli.json) {
        (Some(CliCommand::Exec { json }), _) => {
            let response = handle_request_line(json);
            let code = u8::from(!response.ok);
            emit(&response, cli.pretty);
            code
        }
        (Some(CliCommand::Serve), _) | (None, true) => serve(),
        (Some(_), true) => {
            emit(
                &Response::failure(
                    "invalid_arguments",
                    "--json reads actions from stdin. Drop the subcommand, or use serve or exec",
                ),
                cli.pretty,
            );
            2
        }
        (None, false) => {
            let mut command = Cli::command();
            let _ = command.print_help();
            println!();
            2
        }
        (Some(command), false) => {
            let context = CallContext {
                app: cli.app.clone(),
                window: cli.window.clone(),
                element: cli.element,
                diff: cli.diff,
                full: cli.full,
                session: cli.session.clone(),
                require_visible: cli.require_visible,
                ..CallContext::default()
            };
            match command_action(command, context) {
                Ok((action, context)) => emit_action(action, &context, cli.pretty),
                Err(response) => {
                    emit(&response, cli.pretty);
                    1
                }
            }
        }
    }
}

fn command_action(
    command: &CliCommand,
    mut context: CallContext,
) -> Result<(Action, CallContext), Response> {
    let action = match command {
        CliCommand::Screenshot {
            output,
            region,
            format,
            max_width,
            canvas,
            base64,
        } => Action::Screenshot {
            output: match output {
                Some(path) => Some(
                    path.to_str()
                        .ok_or_else(|| {
                            Response::failure("invalid_arguments", "output path is not valid UTF-8")
                        })?
                        .to_string(),
                ),
                None => None,
            },
            region: match region {
                Some(text) => Some(parse_region(text).map_err(Response::from_error)?),
                None => None,
            },
            format: format.map(ImageFormat::from),
            max_width: *max_width,
            canvas: canvas.clone(),
            base64: *base64,
        },
        CliCommand::Click {
            x,
            y,
            button,
            double,
        } => Action::Click {
            x: *x,
            y: *y,
            button: MouseButton::from(*button),
            double: *double,
        },
        CliCommand::Press { count } => Action::Press { count: *count },
        CliCommand::SetValue { value } => Action::SetValue {
            value: value.clone(),
        },
        CliCommand::SelectText { start, end } => Action::SelectText {
            start: *start,
            end: *end,
        },
        CliCommand::PerformAction { name } => Action::PerformAction { name: name.clone() },
        CliCommand::ScrollTo => Action::ScrollTo,
        CliCommand::Focus => Action::Focus,
        CliCommand::Move { x, y } => Action::Move { x: *x, y: *y },
        CliCommand::Type { text } => Action::Type { text: text.clone() },
        CliCommand::Key { keys } => Action::Key { keys: keys.clone() },
        CliCommand::Scroll {
            x,
            y,
            dx,
            dy,
            direction,
            amount,
        } => Action::Scroll {
            x: *x,
            y: *y,
            dx: *dx,
            dy: *dy,
            direction: direction.map(ScrollDirection::from),
            amount: *amount,
        },
        CliCommand::GetState => Action::GetState,
        CliCommand::Wait { ms, seconds } => Action::Wait {
            ms: *ms,
            seconds: *seconds,
        },
        CliCommand::Displays => Action::Displays,
        CliCommand::ListApps { all } => Action::ListApps { all: *all },
        CliCommand::ListWindows => Action::ListWindows,
        CliCommand::Launch { app } => {
            match (app, context.app.as_deref()) {
                (Some(positional), Some(flag)) if positional != flag => {
                    return Err(Response::failure(
                        "invalid_arguments",
                        "launch app and --app disagree",
                    ));
                }
                (Some(positional), _) => context.app = Some(positional.clone()),
                (None, None) => {
                    return Err(Response::failure(
                        "invalid_arguments",
                        "launch requires an app name, bundle id, path, or pid",
                    ));
                }
                (None, Some(_)) => {}
            }
            Action::Launch
        }
        CliCommand::Activate => {
            if context
                .app
                .as_deref()
                .is_none_or(|app| app.trim().is_empty())
            {
                return Err(Response::failure(
                    "invalid_arguments",
                    "activate requires --app",
                ));
            }
            Action::Activate
        }
        CliCommand::Quit => {
            if context
                .app
                .as_deref()
                .is_none_or(|app| app.trim().is_empty())
            {
                return Err(Response::failure(
                    "invalid_arguments",
                    "quit requires --app",
                ));
            }
            Action::Quit
        }
        CliCommand::Doctor { request } => Action::Doctor { request: *request },
        CliCommand::Tree => Action::Tree,
        CliCommand::Session { command } => match command {
            SessionCommand::End => Action::SessionEnd,
        },
        CliCommand::Serve | CliCommand::Exec { .. } => {
            return Err(Response::failure(
                "invalid_arguments",
                "internal error: serve and exec are not desktop actions",
            ))
        }
    };
    Ok((action, context))
}

fn emit_action(action: Action, context: &CallContext, pretty: bool) -> u8 {
    let response = computer_use::dispatch_context(action, context);
    let code = u8::from(!response.ok);
    emit(&response, pretty);
    code
}

fn serve() -> u8 {
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    loop {
        let line = match lines.next() {
            Some(Ok(line)) => line,
            Some(Err(error)) => {
                let response = Response::from_error(computer_use::ComputerError::Io(error));
                let _ = write!(out, "{}", response.to_json_line(false));
                let _ = out.flush();
                return 1;
            }
            None => return 0,
        };
        if line.trim().is_empty() {
            continue;
        }
        let response = handle_request_line(&line);
        if write!(out, "{}", response.to_json_line(false)).is_err() || out.flush().is_err() {
            return 1;
        }
    }
}

fn emit(response: &Response, pretty: bool) {
    let mut stdout = io::stdout().lock();
    let _ = write!(stdout, "{}", response.to_json_line(pretty));
    let _ = stdout.flush();
}

fn clap_message(error: &clap::Error) -> String {
    strip_ansi(&error.to_string())
}

fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(ch);
    }
    out.trim().to_string()
}

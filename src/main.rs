//! `computer-use` command line interface.
//!
//! Every action prints one JSON object on stdout. `--help` and `--version`
//! stay human-readable. `--json` and `serve` read newline-delimited requests.

#![forbid(unsafe_code)]

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use computer_use::{
    handle_request_line, parse_region, Action, ImageFormat, MouseButton, Response, ScrollDirection,
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

    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// Capture the screen, or a logical region, and write a PNG or JPEG file.
    ///
    /// Example: computer-use screenshot --region 0,0,800,600 --max-width 1280
    Screenshot {
        /// File to write. Defaults to a path in the system temp directory.
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,

        /// Logical region `x,y,width,height` in the same space as click coordinates.
        #[arg(long, value_name = "X,Y,W,H")]
        region: Option<String>,

        /// Image format. Default: png.
        #[arg(long, value_enum)]
        format: Option<CliFormat>,

        /// Scale the image down so its width is at most N pixels.
        #[arg(long, value_name = "N")]
        max_width: Option<u32>,

        /// Add a standard base64 `data` field to the JSON result.
        #[arg(long)]
        base64: bool,
    },

    /// Move the pointer to a logical point and click.
    ///
    /// Example: computer-use click --x 120 --y 240 --button right --double
    Click {
        /// Logical x coordinate.
        #[arg(long)]
        x: i32,
        /// Logical y coordinate.
        #[arg(long)]
        y: i32,
        /// Mouse button. Default: left.
        #[arg(long, value_enum, default_value = "left")]
        button: CliButton,
        /// Click twice.
        #[arg(long)]
        double: bool,
    },

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
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => match error.kind() {
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => error.exit(),
            _ => {
                emit(
                    &Response::failure("invalid_arguments", clap_message(&error)),
                    false,
                );
                return ExitCode::from(2);
            }
        },
    };

    let code = match (&cli.command, cli.json) {
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
        (Some(command), false) => match command_action(command) {
            Ok(action) => emit_action(action, cli.pretty),
            Err(response) => {
                emit(&response, cli.pretty);
                1
            }
        },
    };
    ExitCode::from(code)
}

fn command_action(command: &CliCommand) -> Result<Action, Response> {
    Ok(match command {
        CliCommand::Screenshot {
            output,
            region,
            format,
            max_width,
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
        CliCommand::Serve | CliCommand::Exec { .. } => {
            return Err(Response::failure(
                "invalid_arguments",
                "internal error: serve and exec are not desktop actions",
            ))
        }
    })
}

fn emit_action(action: Action, pretty: bool) -> u8 {
    let response = computer_use::dispatch(action);
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

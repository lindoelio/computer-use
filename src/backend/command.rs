//! Run a short-lived process and stop it if it exceeds a timeout.

use std::io::ErrorKind;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Spawn `program` and collect its output, killing it after `timeout`.
pub(crate) fn run_timeout(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<Output, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().map_err(|error| {
        if error.kind() == ErrorKind::NotFound {
            format!("{program} is not installed or not on PATH")
        } else {
            format!("failed to spawn {program}: {error}")
        }
    })?;
    let pid = child.id();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(child.wait_with_output());
    });
    match receiver.recv_timeout(timeout) {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(format!("{program} failed while waiting: {error}")),
        Err(_) => {
            kill_pid(pid);
            let _ = receiver.recv_timeout(Duration::from_secs(2));
            Err(format!(
                "{program} timed out after {}ms",
                timeout.as_millis()
            ))
        }
    }
}

fn kill_pid(pid: u32) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-9", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

pub(crate) fn stderr_snippet(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > 300 {
        let truncated: String = collapsed.chars().take(300).collect();
        format!("{truncated}...")
    } else if collapsed.is_empty() {
        "no error output".to_string()
    } else {
        collapsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_returns_stdout() {
        #[cfg(unix)]
        let result = run_timeout("echo", &["ok"], Duration::from_secs(3));
        #[cfg(windows)]
        let result = run_timeout("cmd", &["/C", "echo ok"], Duration::from_secs(5));
        let output = result.expect("echo should run");
        assert!(output.status.success());
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("ok"));
    }

    #[test]
    fn missing_program_is_an_error() {
        let result = run_timeout("computer-use-missing-bin-xyz", &[], Duration::from_secs(2));
        assert!(result.is_err());
    }
}

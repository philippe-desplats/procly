//! Copies text to the clipboard of the system, for Copy Port in the menu bar menu.
//!
//! The window is not there to do it when it is closed, and no clipboard plugin is among the ones the spec chose, so the
//! app runs the tool the system ships: `pbcopy` on macOS, `wl-copy` or `xclip` elsewhere. Linux is documented, not
//! tested.

use std::io::{self, Write as _};
use std::process::{Command, Stdio};

/// The tools that copy their standard input to the clipboard, in the order they are tried.
fn tools() -> &'static [(&'static str, &'static [&'static str])] {
    if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else {
        &[("wl-copy", &[]), ("xclip", &["-selection", "clipboard"])]
    }
}

/// Feeds `text` to the standard input of `program` and waits for it.
///
/// A tool that exits before reading all of `text` makes the write fail with a broken pipe, and how it exited is then
/// the error, since it says why the copy did not happen.
fn pipe_to(program: &str, args: &[&str], text: &str) -> io::Result<()> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // The standard input is closed at the end of the statement, so the tool sees the end of `text` before the wait.
    let written = child
        .stdin
        .take()
        .map_or(Ok(()), |mut stdin| stdin.write_all(text.as_bytes()));
    let status = child.wait()?;
    if status.success() {
        written
    } else {
        Err(io::Error::other(format!("{program} exited with {status}")))
    }
}

/// Copies `text` to the clipboard.
///
/// # Errors
///
/// When none of the tools of the system could be run, or the last one failed.
pub(crate) fn copy(text: &str) -> io::Result<()> {
    let mut last = io::Error::other("no clipboard tool is known on this system");
    for (program, args) in tools() {
        match pipe_to(program, args, text) {
            Ok(()) => return Ok(()),
            Err(error) => last = error,
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn the_text_reaches_the_tool_on_its_standard_input_unchanged() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let out = dir.path().join("copied");

        pipe_to("sh", &["-c", "cat > \"$1\"", "sh", &out.display().to_string()], "24001").expect("the copy");

        assert_eq!(fs::read_to_string(out).expect("the copied text"), "24001");
    }

    #[test]
    fn a_tool_that_fails_is_an_error() {
        let error = pipe_to("sh", &["-c", "exit 3"], "24001").expect_err("the tool fails");

        assert!(error.to_string().contains("exited with"), "{error}");
    }

    #[test]
    fn a_tool_that_exits_before_reading_is_reported_by_how_it_exited() {
        // Far more than a pipe holds, so the write is still under way when the tool exits without reading any of it.
        let text = "24001\n".repeat(200_000);

        let error = pipe_to("sh", &["-c", "exit 3"], &text).expect_err("the tool fails");

        assert!(error.to_string().contains("exited with"), "{error}");
    }

    #[test]
    fn a_tool_that_is_not_installed_is_an_error() {
        assert!(pipe_to("procly-no-such-clipboard-tool", &[], "24001").is_err());
    }
}

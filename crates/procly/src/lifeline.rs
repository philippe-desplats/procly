//! The lifeline: how a command of `procly mcp` learns that the server is gone.
//!
//! The server keeps a pipe for its whole life, never writes to it and holds its write end alone, close-on-exec so that
//! no command nor any process of theirs ever holds it. Each command it runs inherits the read end and finds its number
//! in a variable of its environment, which is no part of the documented CLI. The read end reaches end of file when the
//! server ends for any reason, SIGKILL included, and the command then exits at once. macOS has nothing like
//! `PR_SET_PDEATHSIG`, and a pipe works the same on both systems.

use std::fs::File;
use std::io::{self, PipeReader, PipeWriter, Read as _};
use std::os::fd::{AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd, RawFd};

use nix::fcntl::{FcntlArg, FdFlag, fcntl};
use nix::sys::stat::{SFlag, fstat};

/// The variable naming the descriptor of the read end in the environment of a command of `procly mcp`.
pub(crate) const FD_ENV: &str = "PROCLY_LIFELINE_FD";

/// The exit code of a command whose server is gone, 128 plus SIGHUP as a shell reports a hangup. Nobody reads it.
const EXIT_SERVER_GONE: i32 = 129;

/// The server side: the pipe, and what hands its read end to a command.
#[derive(Debug)]
pub(crate) struct Lifeline {
    reader: PipeReader,
    /// Never written to, only held: its closing is the signal.
    _writer: PipeWriter,
}

impl Lifeline {
    pub(crate) fn open() -> io::Result<Self> {
        let (reader, writer) = io::pipe()?;
        Ok(Self {
            reader,
            _writer: writer,
        })
    }

    /// Makes `command` inherit the read end and tells it which descriptor it is.
    pub(crate) fn attach(&self, command: &mut tokio::process::Command) -> io::Result<()> {
        let reader = self.reader.try_clone()?;
        command.env(FD_ENV, reader.as_raw_fd().to_string());
        keep_across_exec(command, reader);
        Ok(())
    }
}

#[expect(
    unsafe_code,
    reason = "std has no safe way to leave a descriptor open across the exec of a command"
)]
fn keep_across_exec(command: &mut tokio::process::Command, reader: PipeReader) {
    // SAFETY: the closure runs between fork and exec and only calls fcntl, which allocates nothing, takes no lock and
    // is async-signal-safe. It touches the descriptor table of the command alone, the server keeps its descriptor
    // close-on-exec.
    unsafe {
        command.pre_exec(move || {
            fcntl(&reader, FcntlArg::F_SETFD(FdFlag::empty()))
                .map(drop)
                .map_err(io::Error::from)
        });
    }
}

/// The command side: exits as soon as the server is gone. Does nothing without a lifeline, or when the descriptor the
/// variable names is not an open pipe, so a `procly` started any other way behaves as it always did.
pub(crate) fn watch() {
    let Some(fd) = std::env::var(FD_ENV)
        .ok()
        .and_then(|number| number.parse::<RawFd>().ok())
        // The standard streams are never the lifeline.
        .filter(|fd| *fd > 2)
    else {
        return;
    };
    let Some(fd) = adopt(fd) else {
        return;
    };
    // Whatever the command starts, the Daemon first among them, must not inherit the lifeline.
    if fcntl(&fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).is_err() {
        return;
    }
    let mut lifeline = File::from(fd);
    // Without its thread the command simply runs on, as it would without a lifeline.
    let _ = std::thread::Builder::new().name("lifeline".to_owned()).spawn(move || {
        let mut ignored = [0_u8; 64];
        loop {
            match lifeline.read(&mut ignored) {
                Ok(0) => std::process::exit(EXIT_SERVER_GONE),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    });
}

/// Takes the ownership of the descriptor `fd` if it is open and is a pipe.
#[expect(
    unsafe_code,
    reason = "a descriptor inherited from the parent can only be adopted by its number"
)]
fn adopt(fd: RawFd) -> Option<OwnedFd> {
    // SAFETY: the borrow only lasts for the probes below, which do not close the descriptor.
    let probe = unsafe { BorrowedFd::borrow_raw(fd) };
    fcntl(probe, FcntlArg::F_GETFD).ok()?;
    let kind = SFlag::from_bits_truncate(fstat(probe).ok()?.st_mode) & SFlag::S_IFMT;
    if kind != SFlag::S_IFIFO {
        return None;
    }
    // SAFETY: the descriptor is open, and the server handed it to this process alone for this very purpose.
    Some(unsafe { OwnedFd::from_raw_fd(fd) })
}

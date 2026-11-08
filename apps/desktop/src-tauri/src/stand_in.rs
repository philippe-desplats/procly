//! The stand-ins for the embedded `procly` that the tests write, then run.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Command;

/// Writes `script` as the executable at `path`.
///
/// Written by this process, the stand-in could still be open for writing in a child that another test forks at that
/// moment, and running it would fail with "Text file busy" until that child execs. `cp` writes it in a process of its
/// own, which no test forks from.
pub(crate) fn write(path: &Path, script: &str) {
    let source = path.with_extension("sh");
    fs::write(&source, script).expect("the source of the stand-in can be written");
    let copied = Command::new("cp").arg(&source).arg(path).status().expect("cp can run");
    assert!(copied.success(), "cp writes the stand-in: {copied}");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("the stand-in can be made executable");
}

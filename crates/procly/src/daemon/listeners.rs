//! The TCP ports a process group listens on, to tell whether a Service listens on its Lease, and every process
//! listening on TCP, to name what holds a port or runs in a Worktree.

use super::process_group::Pid;
use std::collections::BTreeMap;
use std::io::{self, Read as _};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Where `lsof` lives on macOS and on Linux, tried after the `PATH` of the Daemon, which may lack `/usr/sbin`.
const LSOF: [&str; 3] = ["lsof", "/usr/sbin/lsof", "/usr/bin/lsof"];
/// How long a lookup with `lsof` or `docker ps` may run, so that a stuck Docker engine or file system never holds up
/// `up` or `status`.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);
/// How often a running lookup is checked for its end.
const LOOKUP_POLL: Duration = Duration::from_millis(10);

/// The TCP ports the members of the process group `pgid` listen on, sorted, read with `lsof`. `None` when `lsof`
/// cannot run, the caller then cannot tell.
pub(super) fn listening_ports(pgid: Pid) -> Option<Vec<u16>> {
    let pgid = pgid.to_string();
    let output = lsof(&["-nP", "-a", "-g", &pgid, "-iTCP", "-sTCP:LISTEN", "-Fn"])?;
    Some(ports(&String::from_utf8_lossy(&output.stdout)))
}

/// A process listening on TCP, as `lsof` sees it: only the processes of the user on macOS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Listener {
    pub(super) pid: i32,
    pub(super) pgid: i32,
    pub(super) command: String,
    /// Sorted, each once.
    pub(super) ports: Vec<u16>,
}

/// Every process listening on TCP, on `port` only when given. `None` when `lsof` cannot run.
pub(super) fn listeners(port: Option<u16>) -> Option<Vec<Listener>> {
    let address = port.map_or_else(|| "-iTCP".to_owned(), |port| format!("-iTCP:{port}"));
    // `+c 0` prints the whole command name instead of its first 9 characters.
    let output = lsof(&["+c", "0", "-nP", &address, "-sTCP:LISTEN", "-Fpgcn"])?;
    #[cfg_attr(
        not(target_os = "linux"),
        expect(unused_mut, reason = "only the Linux build widens the names")
    )]
    let mut listeners = parse_listeners(&String::from_utf8_lossy(&output.stdout));
    #[cfg(target_os = "linux")]
    for listener in &mut listeners {
        listener.command = widen_command(listener.pid, &listener.command);
    }
    Some(listeners)
}

/// The kernel keeps 15 characters of the name of a command, in `comm`, which is all `lsof` reads on Linux.
#[cfg(any(target_os = "linux", test))]
const COMM_LIMIT: usize = 15;

/// The name `comm` stands for when the kernel cut it, read from the executable or the first argument of the process
/// `pid`, `comm` itself when neither tells more, such as for a process of another user or one that has exited.
#[cfg(target_os = "linux")]
fn widen_command(pid: i32, comm: &str) -> String {
    let executable = std::fs::read_link(format!("/proc/{pid}/exe")).ok();
    let executable = executable.as_deref().and_then(std::path::Path::to_str);
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok();
    let first_argument = cmdline
        .as_deref()
        .and_then(|cmdline| cmdline.split(|byte| *byte == 0).next())
        .and_then(|argument| std::str::from_utf8(argument).ok());
    full_command(comm, executable, first_argument)
}

/// `comm` when it is whole, else the file name of `executable` or of `first_argument` that it is the start of.
///
/// A name that `comm` is not the start of is another program, such as the interpreter of a script, or a name the
/// process gave itself, and it never replaces `comm`. A deleted executable shows as `/path/name (deleted)`.
#[cfg(any(target_os = "linux", test))]
fn full_command(comm: &str, executable: Option<&str>, first_argument: Option<&str>) -> String {
    if comm.len() < COMM_LIMIT {
        return comm.to_owned();
    }
    [
        executable.map(|path| path.strip_suffix(" (deleted)").unwrap_or(path)),
        first_argument,
    ]
    .into_iter()
    .flatten()
    .filter_map(|path| path.rsplit('/').next())
    .find(|name| name.len() > comm.len() && name.starts_with(comm))
    .unwrap_or(comm)
    .to_owned()
}

/// The working directory of each of the processes `pids` that `lsof` can read.
pub(super) fn working_directories(pids: &[i32]) -> BTreeMap<i32, PathBuf> {
    if pids.is_empty() {
        return BTreeMap::new();
    }
    let pids: Vec<String> = pids.iter().map(i32::to_string).collect();
    let Some(output) = lsof(&["-nP", "-a", "-d", "cwd", "-p", &pids.join(","), "-Fpn"]) else {
        return BTreeMap::new();
    };
    let mut directories = BTreeMap::new();
    let mut pid = None;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(value) = line.strip_prefix('p') {
            pid = value.parse().ok();
        } else if let (Some(path), Some(pid)) = (line.strip_prefix('n'), pid) {
            directories.insert(pid, PathBuf::from(path));
        }
    }
    directories
}

/// Runs `lsof` with `args`. `None` when it cannot run or fails, `lsof` exiting with 1 when no file matches.
fn lsof(args: &[&str]) -> Option<Output> {
    for lsof in LSOF {
        let mut command = Command::new(lsof);
        command.args(args).stdin(Stdio::null()).stderr(Stdio::null());
        match bounded_output(&mut command) {
            // Not at this place, try the next one.
            Err(_) => {}
            Ok(output) => {
                return output.filter(|output| output.status.success() || output.status.code() == Some(1));
            }
        }
    }
    None
}

/// Runs `command` and returns what it printed on stdout, `Ok(None)` when it ran past [`LOOKUP_TIMEOUT`] and was
/// killed, and the error when it could not start. Its stderr is the caller's to redirect. Blocking.
pub(super) fn bounded_output(command: &mut Command) -> io::Result<Option<Output>> {
    let mut child = command.stdout(Stdio::piped()).spawn()?;
    let mut stdout = child.stdout.take().expect("stdout is piped above");
    // Read on the side, so that a command printing more than the pipe holds never blocks before it exits.
    let reader = std::thread::spawn(move || {
        let mut printed = Vec::new();
        let _ = stdout.read_to_end(&mut printed);
        printed
    });
    let deadline = Instant::now() + LOOKUP_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(LOOKUP_POLL),
            _ => {
                // Started here, so it is ours to kill.
                let _ = child.kill();
                let _ = child.wait();
                return Ok(None);
            }
        }
    };
    Ok(reader.join().ok().map(|stdout| Output {
        status,
        stdout,
        stderr: Vec::new(),
    }))
}

/// The processes of `lsof -F pgcn` output, one `p` field opening each, sorted by pid.
fn parse_listeners(output: &str) -> Vec<Listener> {
    let mut listeners: Vec<Listener> = Vec::new();
    for line in output.lines() {
        let Some(field) = line.chars().next() else { continue };
        let value = &line[field.len_utf8()..];
        if field == 'p' {
            if let Ok(pid) = value.parse() {
                listeners.push(Listener {
                    pid,
                    pgid: pid,
                    command: String::new(),
                    ports: Vec::new(),
                });
            }
            continue;
        }
        let Some(listener) = listeners.last_mut() else { continue };
        match field {
            'g' => listener.pgid = value.parse().unwrap_or(listener.pid),
            'c' => value.clone_into(&mut listener.command),
            'n' => listener.ports.extend(ports(line)),
            _ => {}
        }
    }
    for listener in &mut listeners {
        listener.ports.sort_unstable();
        listener.ports.dedup();
    }
    listeners.sort_by_key(|listener| listener.pid);
    listeners
}

/// The ports of the `n` fields of `lsof -F n` output, such as `n127.0.0.1:3000`, `n*:3000` or `n[::1]:3000`.
fn ports(output: &str) -> Vec<u16> {
    let mut ports: Vec<u16> = output
        .lines()
        .filter_map(|line| line.strip_prefix('n')?.rsplit(':').next()?.parse().ok())
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_port_of_every_listening_address_once() {
        let output = "p4242\nf3\nn127.0.0.1:5173\nf4\nn[::1]:5173\nf5\nn*:24000\np4243\nf6\nn*:9229\n";
        assert_eq!(ports(output), [5173, 9229, 24000]);
        assert_eq!(ports(""), Vec::<u16>::new());
    }

    #[test]
    fn widens_a_command_the_kernel_cut_to_15_characters_from_its_executable() {
        let cut = "procly-test-ser";
        assert_eq!(
            full_command(cut, Some("/tmp/target/debug/procly-test-server"), None),
            "procly-test-server"
        );
        assert_eq!(
            full_command(cut, Some("/tmp/target/debug/procly-test-server (deleted)"), None),
            "procly-test-server"
        );
    }

    #[test]
    fn widens_a_cut_command_from_the_first_argument_when_the_executable_is_unreadable() {
        let cut = "procly-test-ser";
        assert_eq!(
            full_command(cut, None, Some("./target/debug/procly-test-server")),
            "procly-test-server"
        );
        assert_eq!(
            full_command(cut, None, Some("procly-test-server")),
            "procly-test-server"
        );
    }

    #[test]
    fn keeps_the_command_when_it_is_whole_or_nothing_tells_more() {
        assert_eq!(
            full_command("postgres", Some("/usr/lib/postgresql/16/bin/postgres"), None),
            "postgres"
        );
        assert_eq!(full_command("procly-test-ser", None, None), "procly-test-ser");
        // The interpreter of a script is another program than the one `comm` names.
        assert_eq!(
            full_command("long-script-name", Some("/usr/bin/python3"), Some("python3")),
            "long-script-name"
        );
        // A name the process gave itself is not the start of its executable.
        assert_eq!(
            full_command("node server", Some("/usr/bin/node"), Some("node")),
            "node server"
        );
    }

    #[test]
    fn reads_each_listening_process_with_its_group_command_and_ports() {
        let output =
            "p4242\ng4240\ncnode server\nf3\nn127.0.0.1:5173\nf4\nn[::1]:5173\np17\ng17\ncpostgres\nf5\nn*:5432\n";
        assert_eq!(
            parse_listeners(output),
            [
                Listener {
                    pid: 17,
                    pgid: 17,
                    command: "postgres".to_owned(),
                    ports: vec![5432],
                },
                Listener {
                    pid: 4242,
                    pgid: 4240,
                    command: "node server".to_owned(),
                    ports: vec![5173],
                },
            ]
        );
    }
}

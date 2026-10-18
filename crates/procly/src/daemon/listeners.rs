//! The TCP ports a process group listens on, to tell whether a Service listens on its Lease, and every process
//! listening on TCP, to name what holds a port or runs in a Worktree.

use std::collections::BTreeMap;
use std::io::{self, Read as _};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use nix::unistd::Pid;

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
    Some(parse_listeners(&String::from_utf8_lossy(&output.stdout)))
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

//! The TCP ports a process group listens on, to tell whether a Service listens on its Lease, and every process
//! listening on TCP, to name what holds a port or runs in a Worktree.

use super::process_group::Pid;
use std::collections::{BTreeMap, BTreeSet};
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

/// The TCP ports the members of the process groups `groups` listen on, sorted, read with `lsof`. `None` when `lsof`
/// cannot run, the caller then cannot tell. Nothing listens in no group: an empty set lists no port.
pub(super) fn listening_ports(groups: &BTreeSet<Pid>) -> Option<Vec<u16>> {
    if groups.is_empty() {
        return Some(Vec::new());
    }
    let groups = groups.iter().map(ToString::to_string).collect::<Vec<_>>().join(",");
    let output = lsof(&["-nP", "-a", "-g", &groups, "-iTCP", "-sTCP:LISTEN", "-Fn"])?;
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

/// Where `netstat` lives on macOS, tried after the `PATH` of the Daemon, which may lack `/usr/sbin`.
#[cfg(not(target_os = "linux"))]
const NETSTAT: [&str; 2] = ["netstat", "/usr/sbin/netstat"];

/// Whether a TCP socket listens on `port` at any address, IPv4 or IPv6, whoever owns it, read from the socket table of
/// the kernel: `/proc/net/tcp` and `/proc/net/tcp6` on Linux, `netstat` elsewhere. Unlike `lsof`, it sees the sockets
/// of every user. `None` when the table cannot be read, the caller then cannot tell. Blocking.
pub(super) fn listens_anywhere(port: u16) -> Option<bool> {
    #[cfg(target_os = "linux")]
    {
        let mut listening = false;
        for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
            match std::fs::read_to_string(table) {
                Ok(text) => listening |= proc_net_listens(&text, port),
                // A kernel without IPv6 has no table for it, and nothing listening on it.
                Err(error) if table.ends_with('6') && error.kind() == io::ErrorKind::NotFound => {}
                Err(_) => return None,
            }
        }
        Some(listening)
    }
    #[cfg(not(target_os = "linux"))]
    {
        for netstat in NETSTAT {
            let mut command = Command::new(netstat);
            command
                .args(["-an", "-p", "tcp"])
                .stdin(Stdio::null())
                .stderr(Stdio::null());
            match bounded_output(&mut command) {
                // Not at this place, try the next one.
                Err(_) => {}
                Ok(Some(output)) if output.status.success() => {
                    return Some(netstat_listens(&String::from_utf8_lossy(&output.stdout), port));
                }
                Ok(_) => return None,
            }
        }
        None
    }
}

/// Whether a line of `/proc/net/tcp` or `/proc/net/tcp6` is a socket in the `LISTEN` state, `0A`, on `port`, its
/// local address being `<address>:<port>` in hexadecimal.
#[cfg(any(target_os = "linux", test))]
fn proc_net_listens(table: &str, port: u16) -> bool {
    table.lines().skip(1).any(|line| {
        let mut fields = line.split_whitespace();
        let local = fields.nth(1);
        let state = fields.nth(1);
        state == Some("0A")
            && local
                .and_then(|local| local.rsplit_once(':'))
                .and_then(|(_, hex)| u16::from_str_radix(hex, 16).ok())
                == Some(port)
    })
}

/// Whether a line of `netstat -an -p tcp` is a socket in the `LISTEN` state on `port`, its local address being
/// `<address>.<port>`, such as `*.3000`, `127.0.0.1.3000` or `fe80::1%lo0.3000`.
#[cfg(any(not(target_os = "linux"), test))]
fn netstat_listens(table: &str, port: u16) -> bool {
    table.lines().any(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        fields.first().is_some_and(|proto| proto.starts_with("tcp"))
            && fields.get(5) == Some(&"LISTEN")
            && fields
                .get(3)
                .and_then(|local| local.rsplit_once('.'))
                .and_then(|(_, port)| port.parse::<u16>().ok())
                == Some(port)
    })
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
    fn reads_a_listening_socket_on_any_address_from_netstat_and_skips_other_states() {
        let table = "Active Internet connections (including servers)\n\
            Proto Recv-Q Send-Q  Local Address          Foreign Address        (state)\n\
            tcp4       0      0  127.0.0.1.3000         127.0.0.1.52236        TIME_WAIT\n\
            tcp4       0      0  172.31.0.224.3001      *.*                    LISTEN\n\
            tcp6       0      0  fe80::1%lo0.3002       *.*                    LISTEN\n\
            tcp46      0      0  *.3003                 *.*                    LISTEN\n\
            tcp4       0      0  127.0.0.1.3004         127.0.0.1.52237        ESTABLISHED\n";
        assert!(!netstat_listens(table, 3000));
        assert!(netstat_listens(table, 3001));
        assert!(netstat_listens(table, 3002));
        assert!(netstat_listens(table, 3003));
        assert!(!netstat_listens(table, 3004));
        assert!(!netstat_listens(table, 30));
    }

    #[test]
    fn reads_a_listening_socket_from_proc_net_tcp_and_skips_other_states() {
        // 0x0BB8 is 3000 in TIME_WAIT (06), 0x0BB9 is 3001 in LISTEN (0A).
        let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   \
            0: 0100007F:0BB8 0100007F:CC4C 06 00000000:00000000 03:00000123 00000000     0        0 0\n   \
            1: E0001FAC:0BB9 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4242 1\n";
        assert!(!proc_net_listens(table, 3000));
        assert!(proc_net_listens(table, 3001));
        let table6 = "  sl  local_address                         remote_address                        st\n   \
            0: 000080FE000000000000000001000000:0BBA 00000000000000000000000000000000:0000 0A\n";
        assert!(proc_net_listens(table6, 3002));
    }

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

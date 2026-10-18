//! The TCP ports a process group listens on, to tell whether a Service listens on its Lease.

use std::process::{Command, Stdio};

use nix::unistd::Pid;

/// Where `lsof` lives on macOS and on Linux, tried after the `PATH` of the Daemon, which may lack `/usr/sbin`.
const LSOF: [&str; 3] = ["lsof", "/usr/sbin/lsof", "/usr/bin/lsof"];

/// The TCP ports the members of the process group `pgid` listen on, sorted, read with `lsof`. `None` when `lsof`
/// cannot run, the caller then cannot tell.
pub(super) fn listening_ports(pgid: Pid) -> Option<Vec<u16>> {
    let pgid = pgid.to_string();
    let output = LSOF.iter().find_map(|lsof| {
        Command::new(lsof)
            .args(["-nP", "-a", "-g", &pgid, "-iTCP", "-sTCP:LISTEN", "-Fn"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
    })?;
    // `lsof` exits with 1 when no file matches, which here means that the group listens on no port.
    if !output.status.success() && output.status.code() != Some(1) {
        return None;
    }
    Some(ports(&String::from_utf8_lossy(&output.stdout)))
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
}

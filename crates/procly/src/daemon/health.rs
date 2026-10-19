//! Health checks: a TCP connection to the port of a Service, optionally followed by an HTTP request.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::TcpStream;

use crate::config::HealthConfig;

/// A dev server may listen on IPv4, on IPv6 only (Vite on macOS binds `::1`), or on both.
const LOOPBACKS: [IpAddr; 2] = [IpAddr::V4(Ipv4Addr::LOCALHOST), IpAddr::V6(Ipv6Addr::LOCALHOST)];

/// What makes a Service healthy.
#[derive(Debug, Clone)]
pub(crate) struct HealthCheck {
    /// The port that must accept connections, `None` for a Service that listens on none and is healthy once started.
    pub(crate) port: Option<u16>,
    pub(crate) http: Option<HttpCheck>,
    /// The checks of the Services a Group covers, which all must pass for the Group to be healthy.
    pub(crate) covered: Vec<HealthCheck>,
}

/// An HTTP request sent once the port accepts connections.
#[derive(Debug, Clone)]
pub(crate) struct HttpCheck {
    pub(crate) path: String,
    pub(crate) status: u16,
}

impl HealthCheck {
    /// The check a Service declares, probing `port`, the Lease of its health Port name.
    pub(crate) fn declared(port: Option<u16>, health: &HealthConfig) -> Self {
        Self {
            port,
            http: health.http().map(|(path, status)| HttpCheck { path, status }),
            covered: Vec::new(),
        }
    }

    /// The port of every Lease this check probes, the covered Services' included.
    pub(crate) fn ports(&self) -> Vec<u16> {
        self.port
            .into_iter()
            .chain(self.covered.iter().filter_map(|covered| covered.port))
            .collect()
    }

    /// Probes the Service once, and the Services it covers.
    pub(crate) async fn passes(&self) -> bool {
        if !self.probe().await {
            return false;
        }
        for covered in &self.covered {
            if !covered.probe().await {
                return false;
            }
        }
        true
    }

    /// Probes the port of the check alone.
    async fn probe(&self) -> bool {
        let Some(port) = self.port else { return true };
        for ip in LOOPBACKS {
            let Ok(stream) = TcpStream::connect((ip, port)).await else {
                continue;
            };
            return match &self.http {
                None => true,
                Some(http) => http.status_of(stream, port).await == Some(http.status),
            };
        }
        false
    }
}

impl HttpCheck {
    async fn status_of(&self, mut stream: TcpStream, port: u16) -> Option<u16> {
        let request = format!(
            "GET {} HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n",
            self.path
        );
        stream.write_all(request.as_bytes()).await.ok()?;
        let mut status_line = String::new();
        BufReader::new(stream).read_line(&mut status_line).await.ok()?;
        status(&status_line)
    }
}

/// The status code of an HTTP status line such as `HTTP/1.1 200 OK`.
fn status(status_line: &str) -> Option<u16> {
    let mut fields = status_line.split_ascii_whitespace();
    if !fields.next()?.starts_with("HTTP/") {
        return None;
    }
    fields.next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_status_of_a_status_line() {
        assert_eq!(status("HTTP/1.1 200 OK\r\n"), Some(200));
        assert_eq!(status("HTTP/1.0 503 Service Unavailable\r\n"), Some(503));
        assert_eq!(status("HTTP/2 204\r\n"), Some(204));
    }

    #[test]
    fn rejects_what_is_not_a_status_line() {
        assert_eq!(status(""), None);
        assert_eq!(status("SSH-2.0-OpenSSH_9.6\r\n"), None);
        assert_eq!(status("HTTP/1.1 OK\r\n"), None);
    }
}

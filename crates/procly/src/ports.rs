//! Port names, the Range Leases are drawn from, and Leases as a Service sees them: the placeholders of its command.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::fmt;
use std::str::FromStr;

use crate::placeholder::{self, Piece};

/// What a placeholder for a Port name of the Service itself starts with, such as `{ports.http}`.
const PLACEHOLDER_PREFIX: &str = "ports.";

/// The span of port numbers Leases are drawn from outside the Main worktree, bounds included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Range {
    pub(crate) start: u16,
    pub(crate) end: u16,
}

impl Range {
    /// Below the ephemeral ports of Linux (from 32768) and macOS (from 49152), which the system hands out to
    /// outgoing connections, and above the ports dev servers and databases use by default.
    pub(crate) const DEFAULT: Self = Self {
        start: 24000,
        end: 24999,
    };

    /// Every port of the Range, in increasing order.
    pub(crate) fn ports(self) -> std::ops::RangeInclusive<u16> {
        self.start..=self.end
    }
}

impl FromStr for Range {
    type Err = String;

    /// Parses `<start>-<end>`, such as `24000-24999`.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid = || format!("invalid port range {text:?}, expected <start>-<end> such as \"24000-24999\"");
        let (start, end) = text.split_once('-').ok_or_else(invalid)?;
        let (start, end): (u16, u16) = (
            start.trim().parse().map_err(|_| invalid())?,
            end.trim().parse().map_err(|_| invalid())?,
        );
        if start == 0 || start > end {
            return Err(format!(
                "invalid port range {text:?}, its start must be at least 1 and at most its end"
            ));
        }
        Ok(Self { start, end })
    }
}

impl fmt::Display for Range {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.start, self.end)
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum PortsError {
    #[error(
        "the command of service {service} uses the placeholder {{ports.{name}}}, but {name} is not one of its Port names"
    )]
    UnknownPlaceholder { service: String, name: String },
    #[error("Port name {name:?} of service {service} must be made of lowercase letters, digits and underscores")]
    InvalidPortName { service: String, name: String },
}

/// Checks that each Port name of the Service fits in a variable of its own, and that each placeholder of its
/// command names one of them, before any Lease is granted for it.
pub(crate) fn check<'a>(
    service: &str,
    command: &str,
    port_names: impl IntoIterator<Item = &'a String>,
) -> Result<(), PortsError> {
    let port_names: Vec<&String> = port_names.into_iter().collect();
    // Lowercase only, so that 2 Port names never map to the same variable.
    if let Some(name) = port_names.iter().find(|name| {
        name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    }) {
        return Err(PortsError::InvalidPortName {
            service: service.to_owned(),
            name: (*name).clone(),
        });
    }
    match placeholders(command).find(|name| !port_names.iter().any(|port_name| port_name == name)) {
        Some(name) => Err(PortsError::UnknownPlaceholder {
            service: service.to_owned(),
            name: name.to_owned(),
        }),
        None => Ok(()),
    }
}

/// The variables carrying the Leases of a Service: `PORT` for `health_port`, the Lease its health check probes, and
/// `PORT_<NAME>` for each Port name, such as `PORT_HTTP`.
pub(crate) fn environment(leases: &BTreeMap<String, u16>, health_port: Option<u16>) -> Vec<(String, String)> {
    leases
        .iter()
        .map(|(name, port)| (format!("PORT_{}", name.to_ascii_uppercase()), port.to_string()))
        .chain(health_port.map(|port| ("PORT".to_owned(), port.to_string())))
        .collect()
}

/// Replaces each `{ports.<name>}` of `command` with the leased port of that Port name, once [`check`] has passed.
///
/// Every other brace is left alone, a shell command is full of them (`${VAR}`, `awk '{ print }'`, `find -exec {}`).
pub(crate) fn render(command: &str, leases: &BTreeMap<String, u16>) -> String {
    let Ok(rendered) = placeholder::render(command, |braced| {
        Ok::<_, Infallible>(port_name(braced).and_then(|name| leases.get(name)).map(u16::to_string))
    });
    rendered
}

/// The Port name of each `{ports.<name>}` of `command`, skipping what only looks like one, such as `{ports.}` or
/// `{ports.a b}`.
fn placeholders(command: &str) -> impl Iterator<Item = &str> {
    placeholder::pieces(command)
        .into_iter()
        .filter_map(|piece| match piece {
            Piece::Braced(braced) => port_name(braced),
            Piece::Text(_) => None,
        })
}

/// The Port name a braced name places, `http` for `ports.http`.
fn port_name(braced: &str) -> Option<&str> {
    braced
        .strip_prefix(PLACEHOLDER_PREFIX)
        .filter(|name| !name.is_empty() && name.chars().all(is_port_name_char))
}

/// The characters a placeholder may name, the Port name itself is checked by [`check`].
fn is_port_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leases() -> BTreeMap<String, u16> {
        BTreeMap::from([("http".to_owned(), 24000), ("test".to_owned(), 24001)])
    }

    #[test]
    fn a_range_reads_its_bounds_inclusive() {
        let range: Range = "24000-24999".parse().unwrap();
        assert_eq!(
            range,
            Range {
                start: 24000,
                end: 24999
            }
        );
        assert_eq!(range.ports().count(), 1000);
        assert_eq!(range.to_string(), "24000-24999");
        assert_eq!("8080-8080".parse::<Range>().unwrap().ports().count(), 1);
    }

    #[test]
    fn a_range_must_be_two_ordered_ports() {
        for text in ["24000", "24000-", "a-b", "24999-24000", "0-10", "24000-70000"] {
            assert!(text.parse::<Range>().is_err(), "{text} parsed");
        }
    }

    #[test]
    fn port_carries_the_health_port_and_each_port_name_its_own_variable() {
        assert_eq!(
            environment(&leases(), Some(24000)),
            [
                ("PORT_HTTP".to_owned(), "24000".to_owned()),
                ("PORT_TEST".to_owned(), "24001".to_owned()),
                ("PORT".to_owned(), "24000".to_owned()),
            ]
        );
        assert_eq!(environment(&BTreeMap::new(), None), Vec::<(String, String)>::new());
    }

    #[test]
    fn a_port_name_must_fit_in_one_variable_of_its_own() {
        for name in ["Http", "api-http", ""] {
            let names = [name.to_owned()];
            assert!(
                matches!(check("web", "serve", &names), Err(PortsError::InvalidPortName { .. })),
                "{name:?} was accepted"
            );
        }
        assert!(check("web", "serve", &["api_http2".to_owned()]).is_ok());
    }

    #[test]
    fn each_placeholder_becomes_the_leased_port_of_its_port_name() {
        assert_eq!(
            render("vite --port {ports.http} --strictPort && serve {ports.test}", &leases()),
            "vite --port 24000 --strictPort && serve 24001"
        );
    }

    #[test]
    fn other_braces_of_a_shell_command_are_left_alone() {
        let command = "echo ${PORT} {} {ports.} {ports.a b} {ports.http | awk '{ print $1 }'";
        assert_eq!(render(command, &leases()), command);
        assert!(check("web", command, leases().keys()).is_ok());
    }

    #[test]
    fn a_placeholder_for_an_undeclared_port_name_is_an_error() {
        assert!(matches!(
            check("web", "serve --port {ports.htpp}", leases().keys()),
            Err(PortsError::UnknownPlaceholder { name, .. }) if name == "htpp"
        ));
    }
}

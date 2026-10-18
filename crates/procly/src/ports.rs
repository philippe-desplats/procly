//! Port names, the Range Leases are drawn from, and Leases as a Service sees them: the placeholders of its command.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

/// What opens a placeholder for a Port name of the Service itself, such as `{ports.http}`.
const PLACEHOLDER_PREFIX: &str = "{ports.";

/// The span of port numbers Leases are drawn from outside the Main worktree, bounds included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Range {
    pub(crate) start: u16,
    pub(crate) end: u16,
}

impl Range {
    /// Below the ephemeral ports of Linux (from 32768) and macOS (from 49152), which the system hands out to
    /// outgoing connections, and above the ports dev servers and databases use by default.
    pub(crate) const DEFAULT: Self = Self { start: 24000, end: 24999 };

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
    #[error("the command of service {service} uses the placeholder {{ports.{name}}}, but {name} is not one of its Port names")]
    UnknownPlaceholder { service: String, name: String },
}

/// Replaces each `{ports.<name>}` of `command` with the leased port of that Port name.
///
/// Every other brace is left alone, a shell command is full of them (`${VAR}`, `awk '{ print }'`, `find -exec {}`).
pub(crate) fn render(service: &str, command: &str, leases: &BTreeMap<String, u16>) -> Result<String, PortsError> {
    let mut rendered = String::with_capacity(command.len());
    let mut rest = command;
    while let Some(start) = rest.find(PLACEHOLDER_PREFIX) {
        let after = &rest[start + PLACEHOLDER_PREFIX.len()..];
        let name_len = after
            .find(|c: char| !is_port_name_char(c))
            .unwrap_or(after.len());
        if name_len == 0 || !after[name_len..].starts_with('}') {
            // Not a placeholder, such as `{ports.}` or `{ports.a b}`: copy the opening brace and look further.
            rendered.push_str(&rest[..=start]);
            rest = &rest[start + 1..];
            continue;
        }
        let name = &after[..name_len];
        let port = leases.get(name).ok_or_else(|| PortsError::UnknownPlaceholder {
            service: service.to_owned(),
            name: name.to_owned(),
        })?;
        rendered.push_str(&rest[..start]);
        rendered.push_str(&port.to_string());
        rest = &after[name_len + 1..];
    }
    rendered.push_str(rest);
    Ok(rendered)
}

/// The characters of a Port name, which must also fit in the name of an environment variable.
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
        assert_eq!(range, Range { start: 24000, end: 24999 });
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
    fn each_placeholder_becomes_the_leased_port_of_its_port_name() {
        assert_eq!(
            render("web", "vite --port {ports.http} --strictPort && serve {ports.test}", &leases()).unwrap(),
            "vite --port 24000 --strictPort && serve 24001"
        );
    }

    #[test]
    fn other_braces_of_a_shell_command_are_left_alone() {
        let command = "echo ${PORT} {} {ports.} {ports.a b} {ports.http | awk '{ print $1 }'";
        assert_eq!(render("web", command, &leases()).unwrap(), command);
    }

    #[test]
    fn a_placeholder_for_an_undeclared_port_name_is_an_error() {
        assert!(matches!(
            render("web", "serve --port {ports.htpp}", &leases()),
            Err(PortsError::UnknownPlaceholder { name, .. }) if name == "htpp"
        ));
    }
}

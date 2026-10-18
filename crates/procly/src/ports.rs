//! Port names and their Leases as a Service sees them: the placeholders of its command.

use std::collections::BTreeMap;

/// What opens a placeholder for a Port name of the Service itself, such as `{ports.http}`.
const PLACEHOLDER_PREFIX: &str = "{ports.";

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

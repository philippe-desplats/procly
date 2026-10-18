//! The Compose services proposed from the compose file of a Worktree.
//!
//! The file is read as written and not through `docker compose config`, which resolves the variables the file
//! interpolates, such as `${DB_PORT:-5432}`, and so hides the names Procly passes its Leases through. It also needs
//! no Docker engine to run.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_yaml_ng::Value;

use super::{HealthEntry, Proposal, ProposalError, ResourceEntry, ServiceEntry, service_name};

/// The compose files Docker looks for, in the order it prefers them.
const COMPOSE_FILES: [&str; 4] = [
    "compose.yaml",
    "compose.yml",
    "docker-compose.yaml",
    "docker-compose.yml",
];

/// What Procly knows about a technology: the images it runs from, the Driver that gives each Worktree its own Logical
/// resource in it when a bundled one exists, and the Port name of its well-known ports, the first one being probed.
#[derive(Debug)]
struct Technology {
    /// Image names, as `postgres` in `ghcr.io/acme/postgres:17`. A name matches whole, so that `postgres-exporter`
    /// is not taken for a Postgres.
    images: &'static [&'static str],
    driver: Option<&'static str>,
    ports: &'static [(u16, &'static str)],
}

const TECHNOLOGIES: &[Technology] = &[
    Technology {
        images: &["postgres", "postgresql", "postgis", "pgvector", "timescaledb"],
        driver: Some("postgres"),
        ports: &[(5432, "db")],
    },
    Technology {
        images: &["redis", "redis-stack", "redis-stack-server", "valkey", "keydb"],
        driver: None,
        ports: &[(6379, "redis")],
    },
    Technology {
        images: &["mailpit", "mailhog"],
        driver: None,
        ports: &[(1025, "smtp"), (8025, "http")],
    },
    Technology {
        images: &["mysql", "mariadb"],
        driver: None,
        ports: &[(3306, "db")],
    },
    Technology {
        images: &["mongo"],
        driver: None,
        ports: &[(27017, "db")],
    },
    Technology {
        images: &["minio"],
        driver: None,
        ports: &[(9000, "api"), (9001, "console")],
    },
    Technology {
        images: &["rabbitmq"],
        driver: None,
        ports: &[(5672, "amqp"), (15672, "http")],
    },
];

impl Technology {
    /// The Port name of its well-known port `target`.
    fn port_name(&self, target: u16) -> Option<&'static str> {
        self.ports
            .iter()
            .find(|(known, _)| *known == target)
            .map(|(_, name)| *name)
    }

    /// The first of its well-known Port names that `named` holds, the one its health check probes.
    fn probed<'a>(&self, named: &'a [String]) -> Option<&'a String> {
        self.ports
            .iter()
            .find_map(|(_, name)| named.iter().find(|candidate| candidate == name))
    }
}

/// The technology an image reference runs, `ghcr.io/acme/postgres:17@sha256:…` being `postgres`.
fn technology(image: &str) -> Option<&'static Technology> {
    let name = image.split('@').next()?.rsplit('/').next()?.split(':').next()?;
    TECHNOLOGIES.iter().find(|technology| technology.images.contains(&name))
}

/// A port a compose service publishes on the host.
#[derive(Debug, PartialEq, Eq)]
struct Published {
    /// The variable the compose file reads the port from, as `DB_PORT` in `${DB_PORT:-5432}`.
    variable: Option<String>,
    /// The host port the file writes, or the default of its variable.
    port: u16,
    /// The port in the container.
    target: u16,
}

/// The variable and the default of `NAME:-default` or `NAME-default`, the inside of a `${…}`.
fn split_default(inner: &str) -> Option<(&str, &str)> {
    inner.split_once(":-").or_else(|| inner.split_once('-'))
}

/// The host side of a published port: a number, or a variable with a numeric default.
fn host_port(text: &str) -> Option<(Option<String>, u16)> {
    let text = text.trim();
    if let Ok(port) = text.parse() {
        return Some((None, port));
    }
    let inner = text.strip_prefix("${")?.strip_suffix('}')?;
    let (variable, default) = split_default(inner)?;
    let mut chars = variable.chars();
    let identifier = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    identifier.then_some(())?;
    Some((Some(variable.to_owned()), default.trim().parse().ok()?))
}

/// The parts of a short port mapping, split on the colons outside `${…}` and `[…]`.
fn colon_parts(text: &str) -> Vec<&str> {
    let (mut parts, mut depth, mut from) = (Vec::new(), 0_u32, 0);
    for (offset, c) in text.char_indices() {
        match c {
            '{' | '[' => depth += 1,
            '}' | ']' => depth = depth.saturating_sub(1),
            ':' if depth == 0 => {
                parts.push(&text[from..offset]);
                from = offset + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[from..]);
    parts
}

/// A short mapping such as `"5432:5432"`, `"127.0.0.1:${DB_PORT:-5432}:5432"` or `"6379:6379/tcp"`. A mapping that
/// publishes no host port, a range or a UDP port is not one Procly leases.
fn short_mapping(text: &str) -> Option<Published> {
    let text = match text.rsplit_once('/') {
        Some((mapping, "tcp")) => mapping,
        Some(_) => return None,
        None => text,
    };
    let parts = colon_parts(text);
    let ([host, target] | [_, host, target]) = parts.as_slice() else {
        return None;
    };
    let (variable, port) = host_port(host)?;
    Some(Published {
        variable,
        port,
        target: target.trim().parse().ok()?,
    })
}

/// A long mapping, `{ target: 5432, published: "${DB_PORT:-5432}" }`.
fn long_mapping(mapping: &Value) -> Option<Published> {
    if mapping
        .get("protocol")
        .and_then(Value::as_str)
        .is_some_and(|protocol| protocol != "tcp")
    {
        return None;
    }
    let text = |key: &str| match mapping.get(key)? {
        Value::Number(number) => Some(number.to_string()),
        Value::String(text) => Some(text.clone()),
        _ => None,
    };
    let (variable, port) = host_port(&text("published")?)?;
    Some(Published {
        variable,
        port,
        target: text("target")?.parse().ok()?,
    })
}

/// The ports a compose service publishes.
fn published(service: &Value) -> Vec<Published> {
    service
        .get("ports")
        .and_then(Value::as_sequence)
        .into_iter()
        .flatten()
        .filter_map(|mapping| match mapping {
            Value::String(text) => short_mapping(text),
            Value::Mapping(_) => long_mapping(mapping),
            _ => None,
        })
        .collect()
}

/// The value the compose file writes for the environment variable `name` of a service, whether it writes
/// `environment` as a table or as a list.
fn declared_environment(service: &Value, name: &str) -> Option<String> {
    match service.get("environment")? {
        Value::Mapping(table) => match table.get(name)? {
            Value::String(text) => Some(text.clone()),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        },
        Value::Sequence(list) => list
            .iter()
            .filter_map(Value::as_str)
            .find_map(|entry| entry.strip_prefix(name)?.strip_prefix('=').map(str::to_owned)),
        _ => None,
    }
}

/// The value of the environment variable `name` of a compose service, the default of a `${NAME:-default}` value
/// when that is what the file writes.
fn environment(service: &Value, name: &str) -> Option<String> {
    let value = declared_environment(service, name)?;
    match value.strip_prefix("${").and_then(|rest| rest.strip_suffix('}')) {
        Some(inner) => split_default(inner).map(|(_, default)| default.to_owned()),
        None => Some(value),
    }
}

/// Whether a Postgres service has the credentials of the bundled Driver, which builds its URLs for the user and the
/// password `postgres`. A value the file reads from a variable is not known to be them.
fn has_bundled_credentials(service: &Value) -> bool {
    ["POSTGRES_USER", "POSTGRES_PASSWORD"]
        .iter()
        .all(|name| declared_environment(service, name).is_none_or(|value| value == "postgres"))
}

/// A name for a Logical resource in the Main worktree: lowercase letters, digits and single underscores, starting
/// with a letter and not ending in an underscore, which is what a Driver accepts. `postgres` when nothing is left.
fn resource_name(raw: &str) -> String {
    let mut name = String::new();
    for c in raw.to_lowercase().chars() {
        let c = if c.is_ascii_lowercase() || c.is_ascii_digit() {
            c
        } else {
            '_'
        };
        if c != '_' || !name.is_empty() && !name.ends_with('_') {
            name.push(c);
        }
    }
    let name = name.trim_end_matches('_');
    match name.chars().next() {
        None => "postgres".to_owned(),
        Some(first) if first.is_ascii_digit() => format!("db_{name}"),
        Some(_) => name.to_owned(),
    }
}

/// The Port name of a published port: the one a variable Procly sets spells out (`PORT_DB` is `db`), the well-known
/// name of the port of its technology, or `port` for the only port and `port_<target>` among several.
fn port_name(mapping: &Published, technology: Option<&Technology>, alone: bool) -> String {
    let from_variable = mapping
        .variable
        .as_deref()
        .and_then(|variable| variable.strip_prefix("PORT_"))
        .map(|rest| service_name(&rest.to_lowercase(), "").replace('-', "_"))
        .filter(|name| !name.is_empty());
    let well_known = technology.and_then(|technology| technology.port_name(mapping.target).map(str::to_owned));
    from_variable.or(well_known).unwrap_or_else(|| {
        if alone {
            "port".to_owned()
        } else {
            format!("port_{}", mapping.target)
        }
    })
}

/// The compose file of the Worktree, `None` when it has none.
fn find(worktree: &Path) -> Option<PathBuf> {
    COMPOSE_FILES
        .iter()
        .map(|name| worktree.join(name))
        .find(|path| path.is_file())
}

/// Adds a Compose service for each service of the compose file of the Worktree rooted at `worktree`.
pub(super) fn propose(worktree: &Path, proposal: &mut Proposal) -> Result<(), ProposalError> {
    let Some(path) = find(worktree) else {
        return Ok(());
    };
    let text = fs::read_to_string(&path).map_err(|source| ProposalError::Read {
        path: path.clone(),
        source,
    })?;
    let mut file: Value = serde_yaml_ng::from_str(&text).map_err(|source| ProposalError::Yaml {
        path: path.clone(),
        source,
    })?;
    file.apply_merge().map_err(|source| ProposalError::Yaml {
        path: path.clone(),
        source,
    })?;
    let Some(services) = file.get("services").and_then(Value::as_mapping) else {
        return Err(ProposalError::NoComposeServices { path });
    };
    // A variable can carry the Lease of one Service only: the Env file holds one template for it.
    let mut variables = BTreeSet::new();
    for (name, service) in services {
        let Some(name) = name.as_str() else { continue };
        proposal.add_with(&service_name(name, "compose"), |own| {
            compose_service(name, own, service, &mut variables)
        });
    }
    Ok(())
}

/// The Compose service `own` for the service `name` of the compose file, whose definition is `service`. `variables`
/// holds the ones an earlier service already derives.
fn compose_service(name: &str, own: &str, service: &Value, variables: &mut BTreeSet<String>) -> ServiceEntry {
    let ports = published(service);
    let technology = service.get("image").and_then(Value::as_str).and_then(technology);
    let mut entry = ServiceEntry {
        compose: Some(name.to_owned()),
        ..ServiceEntry::default()
    };
    let mut named = Vec::new();
    for mapping in &ports {
        let mut port_name = port_name(mapping, technology, ports.len() == 1);
        while entry.ports.contains_key(&port_name) {
            port_name.push('_');
        }
        entry.ports.insert(port_name.clone(), mapping.port);
        // Procly sets `PORT` and `PORT_<NAME>` itself, so a compose file reading one needs no Derived variable.
        if let Some(variable) = mapping.variable.as_deref()
            && variable != "PORT"
            && !variable.starts_with("PORT_")
            && variables.insert(variable.to_owned())
        {
            entry
                .env
                .insert(variable.to_owned(), format!("{{services.{own}.ports.{port_name}}}"));
        }
        named.push(port_name);
    }
    if let [first, _, ..] = named.as_slice() {
        // Among several Port names, the health check needs the one to probe, the best known one first.
        let probed = technology
            .and_then(|technology| technology.probed(&named))
            .unwrap_or(first);
        entry.health = Some(HealthEntry { port: probed.clone() });
    }
    // A Logical resource is reached through the Lease and the credentials of its Driver, so a container with no port,
    // or with credentials the bundled Driver does not know, stays Shared.
    if let Some(driver) = technology
        .and_then(|technology| technology.driver)
        .filter(|_| !named.is_empty() && has_bundled_credentials(service))
    {
        entry.driver = Some(driver.to_owned());
        let database = environment(service, "POSTGRES_DB").unwrap_or_default();
        entry.resources.insert(
            "dev".to_owned(),
            ResourceEntry {
                name: resource_name(&database),
            },
        );
    }
    entry
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::Driver;

    fn mapping(variable: Option<&str>, port: u16, target: u16) -> Published {
        Published {
            variable: variable.map(str::to_owned),
            port,
            target,
        }
    }

    #[test]
    fn a_short_mapping_publishes_a_fixed_port_or_the_default_of_a_variable() {
        assert_eq!(short_mapping("5432:5432"), Some(mapping(None, 5432, 5432)));
        assert_eq!(short_mapping("127.0.0.1:5433:5432"), Some(mapping(None, 5433, 5432)));
        assert_eq!(
            short_mapping("${DB_PORT:-5432}:5432"),
            Some(mapping(Some("DB_PORT"), 5432, 5432))
        );
        assert_eq!(
            short_mapping("[::1]:${DB_PORT-5432}:5432/tcp"),
            Some(mapping(Some("DB_PORT"), 5432, 5432))
        );
    }

    #[test]
    fn a_short_mapping_that_publishes_no_single_tcp_host_port_is_not_leased() {
        for text in [
            "5432",
            "${DB_PORT}:5432",
            "${DB_PORT:?set it}:5432",
            "8000-8010:8000",
            "53:53/udp",
            "1:2:3:4",
        ] {
            assert_eq!(short_mapping(text), None, "{text}");
        }
    }

    #[test]
    fn the_environment_of_a_service_is_read_from_a_table_or_a_list_with_the_default_of_a_variable() {
        let table: Value = serde_yaml_ng::from_str("environment:\n  POSTGRES_DB: shop\n  OTHER: 1\n").unwrap();
        let list: Value =
            serde_yaml_ng::from_str("environment:\n  - OTHER=1\n  - POSTGRES_DB=${POSTGRES_DB:-shop}\n").unwrap();
        assert_eq!(environment(&table, "POSTGRES_DB").as_deref(), Some("shop"));
        assert_eq!(environment(&list, "POSTGRES_DB").as_deref(), Some("shop"));
        assert_eq!(environment(&table, "MISSING"), None);
    }

    #[test]
    fn a_resource_name_is_one_a_driver_accepts() {
        for (raw, name) in [
            ("shop", "shop"),
            ("My-App", "my_app"),
            ("a--b", "a_b"),
            ("shop_", "shop"),
            ("2fast", "db_2fast"),
            ("", "postgres"),
            ("---", "postgres"),
        ] {
            assert_eq!(resource_name(raw), name, "{raw}");
        }
    }

    #[test]
    fn an_image_reference_is_matched_by_its_name_only() {
        let driver = |image| technology(image).and_then(|technology| technology.driver);
        assert_eq!(driver("postgres:17"), Some("postgres"));
        assert_eq!(driver("ghcr.io/acme/postgis:16@sha256:abc"), Some("postgres"));
        assert_eq!(driver("bitnami/postgresql"), Some("postgres"));
        assert!(technology("redis/redis-stack:latest").is_some());
        assert!(technology("redisinsight").is_none());
        assert!(technology("prometheuscommunity/postgres-exporter").is_none());
    }

    #[test]
    fn every_driver_the_technologies_name_ships_with_procly() {
        let worktree = tempfile::tempdir().unwrap();
        for driver in TECHNOLOGIES.iter().filter_map(|technology| technology.driver) {
            assert!(Driver::load(driver, worktree.path(), None).is_ok(), "{driver}");
        }
    }
}

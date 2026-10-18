//! The configuration of a Repository: user settings, the versioned `procly.toml` and the git-excluded
//! `procly.local.toml` of the Worktree, merged in that order.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::de::{self, MapAccess, Unexpected, Visitor};
use serde::{Deserialize, Deserializer};
use toml::{Table, Value};

use crate::ports::Range;

/// The versioned configuration at the root of a Worktree.
pub(crate) const VERSIONED_FILE: &str = "procly.toml";
/// The git-excluded configuration at the root of a Worktree, for personal overrides.
pub(crate) const LOCAL_FILE: &str = "procly.local.toml";
/// Overrides the directory of the user settings, used by tests so that they never read the real ones.
pub(crate) const CONFIG_DIR_ENV: &str = "PROCLY_CONFIG_DIR";

#[derive(Debug, thiserror::Error)]
pub(crate) enum ConfigError {
    #[error("cannot read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid TOML in {path}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("service {service} declares several Port names ({}), set health.port to the one to probe", .names.join(", "))]
    AmbiguousHealthPort { service: String, names: Vec<String> },
    #[error("health.port of service {service} is {name}, which is not one of its Port names")]
    UnknownHealthPort { service: String, name: String },
    #[error("service {service} needs {need}, which is not declared")]
    UnknownNeed { service: String, need: String },
    #[error("services need each other in a loop: {}", .cycle.join(" -> "))]
    NeedsCycle { cycle: Vec<String> },
    #[error(
        "pinned.{branch}.ports sets a port for {service}.{port_name}, which is not a Port name of a declared service"
    )]
    UnknownPinnedPort {
        branch: String,
        service: String,
        port_name: String,
    },
    #[error("worktree.copy lists {path:?}, which is not a path inside the Worktree")]
    WorktreeCopyPath { path: String },
    #[error("the user settings {path} holds only [settings], move [{key}] to a procly.toml")]
    UserSettingsScope { path: PathBuf, key: String },
    #[error("service {service} must set exactly one of `run` (a Process service) and `compose` (a Compose service)")]
    ServiceKind { service: String },
    #[error("service {service} is {kind}, `{key}` is for {other} only")]
    KeyOfOtherKind {
        service: String,
        key: &'static str,
        kind: &'static str,
        other: &'static str,
    },
    #[error("service {service} is a Group and must cover at least one service")]
    EmptyGroup { service: String },
    #[error("group {group} covers {service}, which is not declared")]
    UnknownCovered { group: String, service: String },
    #[error("group {group} covers {service}, which is not a Process service that runs on its own")]
    CoveredNotProcess { group: String, service: String },
    #[error("group {group} needs {service}, which it covers, and a Group already starts the Services it covers")]
    GroupNeedsCovered { group: String, service: String },
    #[error("{service} is covered by 2 groups ({}), a service runs under one group at most", .groups.join(" and "))]
    CoveredTwice { service: String, groups: Vec<String> },
    #[error("no service is declared, there is nothing to start")]
    NoServices,
    #[error(
        "service {service} is set to `isolation = \"logical\"` but sets no `driver`, which creates the Logical resources"
    )]
    LogicalWithoutDriver { service: String },
    #[error("service {service} sets `driver` but declares no `resources` for it to create")]
    DriverWithoutResources { service: String },
    #[error(
        "driver {name:?} of service {service} must be a file name: lowercase letters, digits, hyphens and underscores"
    )]
    DriverName { service: String, name: String },
    #[error("service {service} declares `resources` but sets no `driver` to create them")]
    ResourcesWithoutDriver { service: String },
    #[error(
        "resource {resource} of service {service} has {name:?}, which must start with a lowercase letter and hold only lowercase letters, digits and underscores, never 2 in a row and never at the end for a name"
    )]
    ResourceName {
        service: String,
        resource: String,
        name: String,
    },
    #[error("invalid configuration merged from {}", .files.iter().map(|file| file.display().to_string()).collect::<Vec<_>>().join(", "))]
    Invalid {
        files: Vec<PathBuf>,
        #[source]
        source: toml::de::Error,
    },
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    #[serde(default)]
    pub(crate) settings: Settings,
    #[serde(default)]
    pub(crate) services: BTreeMap<String, ServiceConfig>,
    #[serde(default)]
    pub(crate) worktree: WorktreeConfig,
    /// The Pinned branches, by branch name.
    #[serde(default)]
    pub(crate) pinned: BTreeMap<String, PinnedConfig>,
}

/// A Pinned branch: its Leases are bound to the branch name rather than to the Worktree, and never expire.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PinnedConfig {
    /// The explicit port of some Port names, by Service then Port name, preferred over any other port.
    #[serde(default)]
    pub(crate) ports: BTreeMap<String, BTreeMap<String, u16>>,
}

/// What `procly worktree init` does in a fresh Worktree.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorktreeConfig {
    /// Untracked files and directories to copy from the Main worktree when the Worktree lacks them, such as env
    /// files and local certificates. Paths are relative to the root of the Worktree.
    #[serde(default)]
    pub(crate) copy: Vec<String>,
    /// Commands run once, in order, by `/bin/sh -c` at the root of the Worktree.
    #[serde(default)]
    pub(crate) setup: Vec<String>,
}

/// The defaults of every Service, usually set in the user settings.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Settings {
    /// How long `up` waits for a Service to become healthy before stopping it.
    #[serde(default = "default_health_timeout", deserialize_with = "duration")]
    pub(crate) health_timeout: Duration,
    /// How many times a crashed Service is restarted before it is given up on.
    #[serde(default = "default_restart_attempts")]
    pub(crate) restart_attempts: u32,
    /// The delay before the first restart of a crashed Service, doubled before each next one.
    #[serde(default = "default_restart_delay", deserialize_with = "duration")]
    pub(crate) restart_delay: Duration,
    /// Whether a Service that printed nothing and used almost no CPU for `idle_timeout` is stopped.
    #[serde(default = "default_idle_stop")]
    pub(crate) idle_stop: bool,
    /// How long a Service must be idle to be stopped.
    #[serde(default = "default_idle_timeout", deserialize_with = "duration")]
    pub(crate) idle_timeout: Duration,
    /// Where the Leases of Worktrees other than the Main worktree are drawn from.
    #[serde(default = "default_port_range", deserialize_with = "range")]
    pub(crate) port_range: Range,
    /// How long a Lease no Service used is kept before its port returns to the Range, Pinned branches aside.
    #[serde(default = "default_lease_expiry", deserialize_with = "duration")]
    pub(crate) lease_expiry: Duration,
    /// How long a Compose service keeps running after the last Worktree that needed it stopped needing it.
    #[serde(default = "default_grace_period", deserialize_with = "duration")]
    pub(crate) grace_period: Duration,
    /// The Isolation mode of every Compose service that sets a Driver and no `isolation` of its own, Logical when
    /// unset.
    #[serde(default)]
    pub(crate) isolation: Option<IsolationMode>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            health_timeout: default_health_timeout(),
            restart_attempts: default_restart_attempts(),
            restart_delay: default_restart_delay(),
            idle_stop: default_idle_stop(),
            idle_timeout: default_idle_timeout(),
            port_range: default_port_range(),
            lease_expiry: default_lease_expiry(),
            grace_period: default_grace_period(),
            isolation: None,
        }
    }
}

/// A Process service, or a Compose service.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServiceConfig {
    /// The command of a Process service, run by `/bin/sh -c` at the root of the Worktree.
    #[serde(default)]
    pub(crate) run: Option<String>,
    /// The service of the compose file a Compose service maps to.
    #[serde(default)]
    pub(crate) compose: Option<String>,
    /// The Services a Group covers: the Group runs `run` as one unit with the environment of all of them. Only a
    /// Process service can set it.
    #[serde(default)]
    pub(crate) covers: Option<Vec<String>>,
    /// The declared port and the Pool of each Port name.
    #[serde(default)]
    pub(crate) ports: BTreeMap<String, PortConfig>,
    /// Overrides `settings.grace_period` for this Compose service.
    #[serde(default, deserialize_with = "optional_duration")]
    pub(crate) grace_period: Option<Duration>,
    /// Overrides `settings.isolation` for this Compose service.
    #[serde(default)]
    pub(crate) isolation: Option<IsolationMode>,
    /// The Driver that creates the Logical resources of a Compose service in its container.
    #[serde(default)]
    pub(crate) driver: Option<String>,
    /// The Logical resources each Worktree gets in the container, by key.
    #[serde(default)]
    pub(crate) resources: BTreeMap<String, ResourceConfig>,
    #[serde(default)]
    pub(crate) health: HealthConfig,
    /// The Derived variables of the Service: each value is a template over Leases and Worktree facts, rendered into
    /// the environment of the Service and mirrored in the Env file.
    #[serde(default)]
    pub(crate) env: BTreeMap<String, String>,
    /// The Services started first, and healthy, before this one.
    #[serde(default)]
    pub(crate) needs: Vec<String>,
    #[serde(default)]
    pub(crate) restart: RestartConfig,
    #[serde(default)]
    pub(crate) idle: IdleConfig,
}

/// One Port name of a Service: its declared port alone, as in `http = 3000`, or with a Pool, as in
/// `http = { port = 3000, pool = [3100, 3101] }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PortConfig {
    /// The port of the Main worktree.
    pub(crate) port: u16,
    /// The ports tried, in order, before the Range in any other Worktree.
    pub(crate) pool: Vec<u16>,
}

impl<'de> Deserialize<'de> for PortConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        /// The table form, deserialized as declared so that a misspelt key is named in the error.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct PortTable {
            port: u16,
            #[serde(default)]
            pool: Vec<u16>,
        }

        /// Takes the bare number and the table form, which a derive of an untagged enum would accept with an
        /// error naming neither.
        struct PortVisitor;

        impl<'de> Visitor<'de> for PortVisitor {
            type Value = PortConfig;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a port number, or a table with port and pool")
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<PortConfig, E> {
                u16::try_from(value)
                    .map(PortConfig::declared)
                    .map_err(|_| E::invalid_value(Unexpected::Signed(value), &self))
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<PortConfig, E> {
                u16::try_from(value)
                    .map(PortConfig::declared)
                    .map_err(|_| E::invalid_value(Unexpected::Unsigned(value), &self))
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<PortConfig, A::Error> {
                let PortTable { port, pool } = PortTable::deserialize(de::value::MapAccessDeserializer::new(map))?;
                Ok(PortConfig { port, pool })
            }
        }

        deserializer.deserialize_any(PortVisitor)
    }
}

impl PortConfig {
    /// A declared port without a Pool, the meaning of the bare form `http = 3000`.
    pub(crate) fn declared(port: u16) -> Self {
        Self { port, pool: Vec::new() }
    }
}

/// How a Compose service is separated between Worktrees. The Isolated mode, one container per Worktree, is for a later
/// version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IsolationMode {
    /// One container, and every Worktree reads the resources of the Main worktree.
    Shared,
    /// One container, and each Worktree has resources of its own in it.
    Logical,
}

impl<'de> Deserialize<'de> for IsolationMode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct IsolationVisitor;

        impl Visitor<'_> for IsolationVisitor {
            type Value = IsolationMode;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("\"shared\" or \"logical\"")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<IsolationMode, E> {
                match value {
                    "shared" => Ok(IsolationMode::Shared),
                    "logical" => Ok(IsolationMode::Logical),
                    // Named apart so that nobody waits for it to start working.
                    "isolated" => Err(E::custom(
                        "the Isolated mode is not in v0.1, use \"shared\" or \"logical\"",
                    )),
                    other => Err(E::invalid_value(Unexpected::Str(other), &self)),
                }
            }
        }

        deserializer.deserialize_str(IsolationVisitor)
    }
}

/// How long a command Seed may run before it is killed.
const DEFAULT_SEED_TIMEOUT: Duration = Duration::from_mins(10);

/// One Logical resource of a Compose service, created by its Driver in each Worktree.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResourceConfig {
    /// The name in the Main worktree, other Worktrees append their slug to it. Only for a Driver that names its
    /// resources, one that numbers them hands the numbers out itself.
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) seed: Seed,
}

/// The initial content of a new Logical resource.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) enum Seed {
    /// Nothing, the resource starts as the Driver creates it.
    #[default]
    Empty,
    /// A copy of the resource of that name, which already exists in the container.
    Template(String),
    /// What a command run on the host, at the root of the Worktree, leaves in the resource.
    Command { run: String, timeout: Duration },
}

impl Seed {
    /// The resource a new resource is copied from, `None` for the other Seeds.
    pub(crate) fn template(&self) -> Option<&str> {
        match self {
            Self::Template(template) => Some(template),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for Seed {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        /// The table form, deserialized as declared so that a misspelt key is named in the error.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct SeedTable {
            template: Option<String>,
            run: Option<String>,
            #[serde(default, deserialize_with = "optional_duration")]
            timeout: Option<Duration>,
        }

        struct SeedVisitor;

        impl<'de> Visitor<'de> for SeedVisitor {
            type Value = Seed;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("\"empty\", or a table with `template`, or with `run` and an optional `timeout`")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Seed, E> {
                match value {
                    "empty" => Ok(Seed::Empty),
                    other => Err(E::invalid_value(Unexpected::Str(other), &self)),
                }
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Seed, A::Error> {
                let table = SeedTable::deserialize(de::value::MapAccessDeserializer::new(map))?;
                match table {
                    SeedTable {
                        template: Some(name),
                        run: None,
                        timeout: None,
                    } => Ok(Seed::Template(name)),
                    SeedTable {
                        template: None,
                        run: Some(run),
                        timeout,
                    } => Ok(Seed::Command {
                        run,
                        timeout: timeout.unwrap_or(DEFAULT_SEED_TIMEOUT),
                    }),
                    _ => Err(de::Error::custom(
                        "a seed table sets `template`, or `run` with an optional `timeout`, and nothing else",
                    )),
                }
            }
        }

        deserializer.deserialize_any(SeedVisitor)
    }
}

/// Overrides `settings.idle_stop` and `settings.idle_timeout` for one Service.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IdleConfig {
    /// `false` keeps the Service running however long it stays idle.
    pub(crate) stop: Option<bool>,
    #[serde(default, deserialize_with = "optional_duration")]
    pub(crate) timeout: Option<Duration>,
}

/// Overrides `settings.restart_attempts` and `settings.restart_delay` for one Service.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RestartConfig {
    pub(crate) attempts: Option<u32>,
    #[serde(default, deserialize_with = "optional_duration")]
    pub(crate) delay: Option<Duration>,
}

/// How a Service that crashed after it was healthy is restarted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RestartPolicy {
    /// How many restarts in a row are tried before the Service is given up on, 0 never restarts it.
    pub(crate) attempts: u32,
    /// The delay before the first restart.
    pub(crate) delay: Duration,
}

impl RestartPolicy {
    /// The delay before the `restart`th restart in a row, counting from 1: the delay doubles each time.
    pub(crate) fn delay_before(&self, restart: u32) -> Duration {
        // Capped well below overflow, a delay doubled 31 times is years long already.
        let doublings = restart.saturating_sub(1).min(31);
        self.delay.saturating_mul(1 << doublings)
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HealthConfig {
    /// The Port name to probe, needed only when the Service declares several.
    #[serde(rename = "port")]
    pub(crate) port_name: Option<String>,
    /// The path of an HTTP request sent once the port accepts connections, `/` when only `status` is set.
    pub(crate) path: Option<String>,
    /// The status that HTTP request must answer, 200 when only `path` is set.
    pub(crate) status: Option<u16>,
    /// Overrides `settings.health_timeout` for this Service.
    #[serde(default, deserialize_with = "optional_duration")]
    pub(crate) timeout: Option<Duration>,
}

impl WorktreeConfig {
    /// Fails on a `copy` entry that is empty, absolute or climbs out of the Worktree, so that no entry can read or
    /// write elsewhere than the Main worktree and the Worktree.
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        for path in &self.copy {
            let components = Path::new(path).components();
            let inside = components
                .clone()
                .all(|part| matches!(part, Component::Normal(_) | Component::CurDir));
            if !inside || !components.into_iter().any(|part| matches!(part, Component::Normal(_))) {
                return Err(ConfigError::WorktreeCopyPath { path: path.clone() });
            }
        }
        Ok(())
    }
}

/// What a Service runs, once its keys are known to agree.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ServiceKind<'a> {
    Process {
        run: &'a str,
    },
    /// A Process service that runs the Services it covers as one unit.
    Group {
        run: &'a str,
        covers: &'a [String],
    },
    /// A service of the compose file of the Repository.
    Compose {
        service: &'a str,
    },
}

impl ServiceConfig {
    /// Whether the Service `name` is a Process service or a Compose service, failing when its keys disagree: both or
    /// neither of `run` and `compose`, or a key that belongs to the other kind.
    pub(crate) fn kind(&self, name: &str) -> Result<ServiceKind<'_>, ConfigError> {
        let wrong_key = |key, kind, other| ConfigError::KeyOfOtherKind {
            service: name.to_owned(),
            key,
            kind,
            other,
        };
        match (&self.run, &self.compose) {
            (Some(run), None) => {
                if self.grace_period.is_some() {
                    return Err(wrong_key("grace_period", "a Process service", "a Compose service"));
                }
                if self.driver.is_some() {
                    return Err(wrong_key("driver", "a Process service", "a Compose service"));
                }
                if self.isolation.is_some() {
                    return Err(wrong_key("isolation", "a Process service", "a Compose service"));
                }
                if !self.resources.is_empty() {
                    return Err(wrong_key("resources", "a Process service", "a Compose service"));
                }
                let Some(covers) = &self.covers else {
                    return Ok(ServiceKind::Process { run });
                };
                // Its Leases and Derived variables are the ones of the Services it covers.
                let group = "a Group";
                let other = "the Services a Group covers";
                if !self.ports.is_empty() {
                    return Err(wrong_key("ports", group, other));
                }
                if !self.env.is_empty() {
                    return Err(wrong_key("env", group, other));
                }
                // Healthy once the Services it covers are, only the timeout is its own.
                if self.health.port_name.is_some() || self.health.http().is_some() {
                    return Err(wrong_key("health", group, other));
                }
                if covers.is_empty() {
                    return Err(ConfigError::EmptyGroup {
                        service: name.to_owned(),
                    });
                }
                Ok(ServiceKind::Group { run, covers })
            }
            (None, Some(service)) => {
                if self.covers.is_some() {
                    return Err(wrong_key("covers", "a Compose service", "a Group"));
                }
                let restart = &self.restart;
                if restart.attempts.is_some() || restart.delay.is_some() {
                    return Err(wrong_key("restart", "a Compose service", "a Process service"));
                }
                let idle = &self.idle;
                if idle.stop.is_some() || idle.timeout.is_some() {
                    return Err(wrong_key("idle", "a Compose service", "a Process service"));
                }
                self.validate_resources(name)?;
                Ok(ServiceKind::Compose { service })
            }
            _ => Err(ConfigError::ServiceKind {
                service: name.to_owned(),
            }),
        }
    }

    /// Fails on a Driver without resources or resources without a Driver, and on a key or a name that is not safe to
    /// put in a command run inside the container.
    fn validate_resources(&self, name: &str) -> Result<(), ConfigError> {
        if self.isolation == Some(IsolationMode::Logical) && self.driver.is_none() {
            return Err(ConfigError::LogicalWithoutDriver {
                service: name.to_owned(),
            });
        }
        match (&self.driver, self.resources.is_empty()) {
            (Some(_), true) => {
                return Err(ConfigError::DriverWithoutResources {
                    service: name.to_owned(),
                });
            }
            (None, false) => {
                return Err(ConfigError::ResourcesWithoutDriver {
                    service: name.to_owned(),
                });
            }
            _ => {}
        }
        if let Some(driver) = &self.driver
            && !is_driver_name(driver)
        {
            return Err(ConfigError::DriverName {
                service: name.to_owned(),
                name: driver.clone(),
            });
        }
        for (key, resource) in &self.resources {
            let template = resource.seed.template();
            // The name is followed by `__` and the slug in other Worktrees, a template is an existing name as it is.
            let unsafe_name = std::iter::once(key.as_str())
                .chain(template)
                .find(|text| !is_resource_name(text))
                .or_else(|| resource.name.as_deref().filter(|name| !is_declared_name(name)));
            if let Some(text) = unsafe_name {
                return Err(ConfigError::ResourceName {
                    service: name.to_owned(),
                    resource: key.clone(),
                    name: text.to_owned(),
                });
            }
        }
        Ok(())
    }

    /// The Port name the health check of the Service `name` probes, `None` when it declares no Port name.
    pub(crate) fn health_port_name(&self, name: &str) -> Result<Option<&str>, ConfigError> {
        match &self.health.port_name {
            Some(port_name) if self.ports.contains_key(port_name) => Ok(Some(port_name)),
            Some(port_name) => Err(ConfigError::UnknownHealthPort {
                service: name.to_owned(),
                name: port_name.clone(),
            }),
            None if self.ports.len() > 1 => Err(ConfigError::AmbiguousHealthPort {
                service: name.to_owned(),
                names: self.ports.keys().cloned().collect(),
            }),
            None => Ok(self.ports.keys().next().map(String::as_str)),
        }
    }
}

impl HealthConfig {
    /// The path and status of the HTTP check, `None` when the TCP connection alone makes the Service healthy.
    pub(crate) fn http(&self) -> Option<(String, u16)> {
        if self.path.is_none() && self.status.is_none() {
            return None;
        }
        Some((
            self.path.clone().unwrap_or_else(|| "/".to_owned()),
            self.status.unwrap_or(200),
        ))
    }
}

impl Config {
    /// How `service` is restarted when it crashes.
    pub(crate) fn restart_policy(&self, service: &ServiceConfig) -> RestartPolicy {
        RestartPolicy {
            attempts: service.restart.attempts.unwrap_or(self.settings.restart_attempts),
            delay: service.restart.delay.unwrap_or(self.settings.restart_delay),
        }
    }

    /// How long `service` may stay idle before it is stopped, `None` when it never is.
    pub(crate) fn idle_timeout(&self, service: &ServiceConfig) -> Option<Duration> {
        service
            .idle
            .stop
            .unwrap_or(self.settings.idle_stop)
            .then(|| service.idle.timeout.unwrap_or(self.settings.idle_timeout))
    }

    /// The Group that covers `service`, if any.
    pub(crate) fn group_of(&self, service: &str) -> Option<&str> {
        self.services
            .iter()
            .find(|(_, declared)| {
                declared
                    .covers
                    .as_ref()
                    .is_some_and(|covers| covers.iter().any(|c| c == service))
            })
            .map(|(name, _)| name.as_str())
    }

    /// The Services to bring up for `service`, each one after everything it needs and `service` last. A Service
    /// needed through several paths appears once.
    ///
    /// A Group starts what it needs and what the Services it covers need, but not the Services it covers: they run
    /// as the Group, and a need that points to a Service of the same Group is the Group itself.
    pub(crate) fn start_order(&self, service: &str) -> Result<Vec<String>, ConfigError> {
        let mut order = Vec::new();
        self.visit(service, &mut Vec::new(), &mut order)?;
        Ok(order)
    }

    /// The Services `up` without arguments brings up: every declared Service that no Group covers, a Group standing
    /// for the Services it covers, each one after everything it needs.
    pub(crate) fn start_order_all(&self) -> Result<Vec<String>, ConfigError> {
        if self.services.is_empty() {
            return Err(ConfigError::NoServices);
        }
        let mut order = Vec::new();
        for name in self.services.keys().filter(|name| self.group_of(name).is_none()) {
            self.visit(name, &mut Vec::new(), &mut order)?;
        }
        Ok(order)
    }

    /// What `dependent` starts to have its need `need` running: the Group covering `need` when the dependent is
    /// outside that Group, since a covered Service runs as the Group and a Group never starts over it running alone.
    fn standing_for<'a>(&'a self, need: &'a str, dependent: &str) -> &'a str {
        match self.group_of(need) {
            Some(group) if group != dependent && self.group_of(dependent) != Some(group) => group,
            _ => need,
        }
    }

    /// Depth-first walk of the needs of `name`, `path` being the Services being visited above it.
    fn visit(&self, name: &str, path: &mut Vec<String>, order: &mut Vec<String>) -> Result<(), ConfigError> {
        if let Some(start) = path.iter().position(|visiting| visiting == name) {
            let mut cycle = path[start..].to_vec();
            cycle.push(name.to_owned());
            return Err(ConfigError::NeedsCycle { cycle });
        }
        if order.iter().any(|done| done == name) {
            return Ok(());
        }
        let Some(declared) = self.services.get(name) else {
            // The caller checks the requested Service, so only a need can be missing here.
            let service = path.last().cloned().unwrap_or_default();
            return Err(ConfigError::UnknownNeed {
                service,
                need: name.to_owned(),
            });
        };
        path.push(name.to_owned());
        for need in &declared.needs {
            self.visit(self.standing_for(need, name), path, order)?;
        }
        for covered in declared.covers.iter().flatten() {
            let Some(covered) = self.services.get(covered) else {
                continue;
            };
            for need in covered.needs.iter().filter(|need| self.group_of(need) != Some(name)) {
                self.visit(self.standing_for(need, name), path, order)?;
            }
        }
        path.pop();
        order.push(name.to_owned());
        Ok(())
    }

    /// Every port the configuration declares, Pools and explicit ports of Pinned branches included, which the
    /// Range skips so that it never hands out a port a Worktree expects.
    pub(crate) fn declared_ports(&self) -> BTreeSet<u16> {
        let services = self
            .services
            .values()
            .flat_map(|service| service.ports.values())
            .flat_map(|port| std::iter::once(port.port).chain(port.pool.iter().copied()));
        let pinned = self
            .pinned
            .values()
            .flat_map(|pinned| pinned.ports.values())
            .flat_map(|ports| ports.values().copied());
        services.chain(pinned).collect()
    }

    /// Fails on an explicit port of a Pinned branch set for a Service or a Port name that is not declared, so
    /// that a typo never leaves a branch without the port it expects.
    fn validate_pinned(&self) -> Result<(), ConfigError> {
        for (branch, pinned) in &self.pinned {
            for (service, ports) in &pinned.ports {
                let declared = self.services.get(service);
                if let Some(port_name) = ports
                    .keys()
                    .find(|port_name| declared.is_none_or(|declared| !declared.ports.contains_key(*port_name)))
                {
                    return Err(ConfigError::UnknownPinnedPort {
                        branch: branch.clone(),
                        service: service.clone(),
                        port_name: port_name.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Fails on a Group that covers a Service that is not declared, not a Process service that runs on its own, or
    /// that another Group covers too, so that a Group and the Services it covers never overlap.
    fn validate_groups(&self) -> Result<(), ConfigError> {
        let mut covered_by: BTreeMap<&str, &str> = BTreeMap::new();
        for (group, declared) in &self.services {
            if let Some(service) = declared
                .needs
                .iter()
                .find(|need| declared.covers.iter().flatten().any(|covered| covered == *need))
            {
                return Err(ConfigError::GroupNeedsCovered {
                    group: group.clone(),
                    service: service.clone(),
                });
            }
            for service in declared.covers.iter().flatten() {
                match self.services.get(service) {
                    None => {
                        return Err(ConfigError::UnknownCovered {
                            group: group.clone(),
                            service: service.clone(),
                        });
                    }
                    Some(covered) if covered.run.is_none() || covered.covers.is_some() => {
                        return Err(ConfigError::CoveredNotProcess {
                            group: group.clone(),
                            service: service.clone(),
                        });
                    }
                    Some(_) => {}
                }
                if let Some(other) = covered_by.insert(service, group) {
                    let mut groups = vec![group.clone(), other.to_owned()];
                    groups.sort();
                    return Err(ConfigError::CoveredTwice {
                        service: service.clone(),
                        groups,
                    });
                }
            }
        }
        Ok(())
    }

    /// How long the Compose service `service` keeps running once no Worktree needs it.
    pub(crate) fn grace_period(&self, service: &ServiceConfig) -> Duration {
        service.grace_period.unwrap_or(self.settings.grace_period)
    }

    /// How the Compose service `service` is separated between Worktrees: the mode it sets, else the one of the
    /// Repository, else Logical. A Service without a Driver has no resources to separate, so it is Shared.
    pub(crate) fn isolation(&self, service: &ServiceConfig) -> IsolationMode {
        if service.driver.is_none() {
            return IsolationMode::Shared;
        }
        service
            .isolation
            .or(self.settings.isolation)
            .unwrap_or(IsolationMode::Logical)
    }

    /// Fails on the first Service whose keys disagree, so that no `up` starts anything from a configuration that
    /// cannot be run.
    fn validate(&self) -> Result<(), ConfigError> {
        self.services
            .iter()
            .try_for_each(|(name, service)| service.kind(name).map(|_| ()))
    }

    /// How long `up` waits for `service` to become healthy.
    pub(crate) fn health_timeout(&self, service: &ServiceConfig) -> Duration {
        service.health.timeout.unwrap_or(self.settings.health_timeout)
    }
}

/// Whether `text` can name a resource in the Main worktree: a resource name that holds no `__` and does not end with
/// `_`, so that the `__` before the slug is always the first one.
fn is_declared_name(text: &str) -> bool {
    is_resource_name(text) && !text.contains("__") && !text.ends_with('_')
}

/// Whether `text` can name the file of a Driver without leaving the directory that holds it.
fn is_driver_name(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Whether `text` is a lowercase letter then lowercase letters, digits and underscores: a word every Driver can use as
/// a resource name, and one that needs no quoting in a shell command.
fn is_resource_name(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Long enough for a dev server compiling a large app on its first start.
fn default_health_timeout() -> Duration {
    Duration::from_mins(1)
}

fn default_idle_stop() -> bool {
    true
}

/// Long enough to cover an agent thinking between 2 uses of its dev server.
fn default_idle_timeout() -> Duration {
    Duration::from_mins(30)
}

fn default_restart_attempts() -> u32 {
    5
}

fn default_restart_delay() -> Duration {
    Duration::from_secs(1)
}

fn default_port_range() -> Range {
    Range::DEFAULT
}

/// Long enough for an agent coming back from a short break to find the container warm.
fn default_grace_period() -> Duration {
    Duration::from_mins(10)
}

/// Long enough to cover a branch left aside during a holiday.
fn default_lease_expiry() -> Duration {
    Duration::from_hours(30 * 24)
}

/// A Range written as `<start>-<end>`, such as `24000-24999`.
fn range<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Range, D::Error> {
    String::deserialize(deserializer)?
        .parse()
        .map_err(serde::de::Error::custom)
}

/// A duration written as `procly run --timeout` accepts it, such as `90s` or `5m`.
fn duration<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
    crate::duration::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
}

fn optional_duration<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Duration>, D::Error> {
    duration(deserializer).map(Some)
}

/// The user settings file: `$PROCLY_CONFIG_DIR/config.toml`, then `$XDG_CONFIG_HOME/procly/config.toml`, then
/// `~/.config/procly/config.toml`. `None` when none of these variables is set.
pub(crate) fn user_settings() -> Option<PathBuf> {
    let dir = if let Some(dir) = non_empty_env(CONFIG_DIR_ENV) {
        dir
    } else if let Some(dir) = non_empty_env("XDG_CONFIG_HOME") {
        dir.join("procly")
    } else {
        non_empty_env("HOME")?.join(".config/procly")
    };
    Some(dir.join("config.toml"))
}

/// The configuration of the Worktree rooted at `worktree`: the user settings, then its versioned file, then its
/// local file, each one overriding the previous key by key. A missing file counts as empty.
pub(crate) fn load(user_settings: Option<&Path>, worktree: &Path) -> Result<Config, ConfigError> {
    let mut merged = Table::new();
    let mut files = Vec::new();
    if let Some(path) = user_settings
        && let Some(table) = read(path)?
    {
        // Defaults only: a Service declared here would appear in every Repository.
        if let Some(key) = table.keys().find(|key| *key != "settings") {
            return Err(ConfigError::UserSettingsScope {
                path: path.to_owned(),
                key: key.clone(),
            });
        }
        merge(&mut merged, table);
        files.push(path.to_owned());
    }
    for path in [worktree.join(VERSIONED_FILE), worktree.join(LOCAL_FILE)] {
        if let Some(table) = read(&path)? {
            merge(&mut merged, table);
            files.push(path);
        }
    }
    let config: Config = Value::Table(merged)
        .try_into()
        .map_err(|source| ConfigError::Invalid { files, source })?;
    config.worktree.validate()?;
    config.validate()?;
    config.validate_groups()?;
    config.validate_pinned()?;
    Ok(config)
}

fn read(path: &Path) -> Result<Option<Table>, ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ConfigError::Read {
                path: path.to_owned(),
                source,
            });
        }
    };
    text.parse().map(Some).map_err(|source| ConfigError::Parse {
        path: path.to_owned(),
        source,
    })
}

/// Merges `overlay` into `base`: tables merge key by key, any other value replaces the one below.
fn merge(base: &mut Table, overlay: Table) {
    for (key, value) in overlay {
        match (base.get_mut(&key), value) {
            (Some(Value::Table(below)), Value::Table(above)) => merge(below, above),
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

fn non_empty_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(text: &str) -> Table {
        text.parse().unwrap()
    }

    fn service(text: &str) -> ServiceConfig {
        Value::Table(table(text)).try_into().unwrap()
    }

    #[test]
    fn the_health_check_probes_the_only_port_or_none() {
        assert_eq!(
            service("run = 'x'\nports = { http = 3000 }\n")
                .health_port_name("web")
                .unwrap(),
            Some("http")
        );
        assert_eq!(service("run = 'x'\n").health_port_name("worker").unwrap(), None);
    }

    #[test]
    fn the_health_check_probes_the_named_port_among_several() {
        let web = service("run = 'x'\nports = { http = 3000, test = 3001 }\nhealth = { port = 'test' }\n");
        assert_eq!(web.health_port_name("web").unwrap(), Some("test"));
    }

    #[test]
    fn the_health_port_must_be_named_among_several_and_declared() {
        let ambiguous = service("run = 'x'\nports = { http = 3000, test = 3001 }\n");
        assert!(matches!(
            ambiguous.health_port_name("web"),
            Err(ConfigError::AmbiguousHealthPort { names, .. }) if names == ["http", "test"]
        ));
        let unknown = service("run = 'x'\nports = { http = 3000 }\nhealth = { port = 'api' }\n");
        assert!(matches!(
            unknown.health_port_name("web"),
            Err(ConfigError::UnknownHealthPort { name, .. }) if name == "api"
        ));
    }

    fn parsed(text: &str) -> Config {
        Value::Table(table(text)).try_into().unwrap()
    }

    #[test]
    fn the_needs_of_a_service_start_before_it_and_once_each() {
        let config = parsed(
            "[services.web]\nrun = 'x'\nneeds = ['api', 'cache']\n\
             [services.api]\nrun = 'x'\nneeds = ['db', 'cache']\n\
             [services.cache]\nrun = 'x'\n\
             [services.db]\nrun = 'x'\n",
        );
        assert_eq!(config.start_order("web").unwrap(), ["db", "cache", "api", "web"]);
        assert_eq!(config.start_order("cache").unwrap(), ["cache"]);
    }

    #[test]
    fn a_need_that_is_not_declared_is_an_error_naming_both_services() {
        let config = parsed("[services.web]\nrun = 'x'\nneeds = ['db']\n");
        assert!(matches!(
            config.start_order("web"),
            Err(ConfigError::UnknownNeed { service, need }) if service == "web" && need == "db"
        ));
    }

    #[test]
    fn needs_that_loop_back_are_an_error_listing_the_cycle() {
        let config = parsed(
            "[services.web]\nrun = 'x'\nneeds = ['api']\n\
             [services.api]\nrun = 'x'\nneeds = ['db']\n\
             [services.db]\nrun = 'x'\nneeds = ['api']\n",
        );
        assert!(matches!(
            config.start_order("web"),
            Err(ConfigError::NeedsCycle { cycle }) if cycle == ["api", "db", "api"]
        ));
        let itself = parsed("[services.web]\nrun = 'x'\nneeds = ['web']\n");
        assert!(matches!(
            itself.start_order("web"),
            Err(ConfigError::NeedsCycle { cycle }) if cycle == ["web", "web"]
        ));
    }

    #[test]
    fn a_service_restarts_five_times_from_one_second_unless_configured_otherwise() {
        let config = parsed("[services.web]\nrun = 'x'\n");
        assert_eq!(
            config.restart_policy(&config.services["web"]),
            RestartPolicy {
                attempts: 5,
                delay: Duration::from_secs(1)
            }
        );
    }

    #[test]
    fn the_restart_of_a_service_overrides_the_settings_key_by_key() {
        let config = parsed(
            "[settings]\nrestart_attempts = 3\nrestart_delay = '2s'\n\
             [services.web]\nrun = 'x'\nrestart = { attempts = 0 }\n\
             [services.api]\nrun = 'x'\nrestart = { delay = '500ms' }\n\
             [services.db]\nrun = 'x'\n",
        );
        let policy = |name: &str| config.restart_policy(&config.services[name]);
        assert_eq!(
            (policy("web").attempts, policy("web").delay),
            (0, Duration::from_secs(2))
        );
        assert_eq!(
            (policy("api").attempts, policy("api").delay),
            (3, Duration::from_millis(500))
        );
        assert_eq!((policy("db").attempts, policy("db").delay), (3, Duration::from_secs(2)));
    }

    #[test]
    fn the_delay_before_a_restart_doubles_each_time() {
        let policy = RestartPolicy {
            attempts: 5,
            delay: Duration::from_secs(1),
        };
        let delays: Vec<_> = (1..=5).map(|restart| policy.delay_before(restart)).collect();
        assert_eq!(delays, [1, 2, 4, 8, 16].map(Duration::from_secs),);
        // Never overflows, however many restarts are allowed.
        assert!(policy.delay_before(u32::MAX) >= Duration::from_secs(16));
    }

    #[test]
    fn a_service_stops_after_30_minutes_idle_unless_configured_otherwise() {
        let config = parsed("[services.web]\nrun = 'x'\n");
        assert_eq!(
            config.idle_timeout(&config.services["web"]),
            Some(Duration::from_mins(30))
        );
    }

    #[test]
    fn the_idle_stop_of_a_service_overrides_the_settings_key_by_key() {
        let config = parsed(
            "[settings]\nidle_timeout = '1h'\n\
             [services.web]\nrun = 'x'\nidle = { timeout = '5m' }\n\
             [services.api]\nrun = 'x'\nidle = { stop = false }\n\
             [services.db]\nrun = 'x'\n",
        );
        let idle = |name: &str| config.idle_timeout(&config.services[name]);
        assert_eq!(idle("web"), Some(Duration::from_mins(5)));
        assert_eq!(idle("api"), None);
        assert_eq!(idle("db"), Some(Duration::from_hours(1)));

        let off = parsed("[settings]\nidle_stop = false\n[services.web]\nrun = 'x'\nidle = { stop = true }\n");
        assert_eq!(off.idle_timeout(&off.services["web"]), Some(Duration::from_mins(30)));
        let off = parsed("[settings]\nidle_stop = false\n[services.web]\nrun = 'x'\n");
        assert_eq!(off.idle_timeout(&off.services["web"]), None);
    }

    #[test]
    fn a_service_is_a_process_by_its_run_and_a_compose_service_by_its_compose() {
        let config = parsed("[services.web]\nrun = 'serve'\n[services.db]\ncompose = 'postgres'\n");
        assert_eq!(
            config.services["web"].kind("web").unwrap(),
            ServiceKind::Process { run: "serve" }
        );
        assert_eq!(
            config.services["db"].kind("db").unwrap(),
            ServiceKind::Compose { service: "postgres" }
        );
    }

    #[test]
    fn a_service_sets_exactly_one_of_run_and_compose() {
        for text in ["ports = { http = 3000 }\n", "run = 'x'\ncompose = 'db'\n"] {
            assert!(
                matches!(
                    service(text).kind("db"),
                    Err(ConfigError::ServiceKind { service }) if service == "db"
                ),
                "{text:?} was accepted"
            );
        }
    }

    #[test]
    fn a_compose_service_refuses_the_keys_of_a_process_and_the_other_way_round() {
        let key = |text: &str, name: &str| match service(text).kind(name) {
            Err(ConfigError::KeyOfOtherKind { key, .. }) => key,
            other => panic!("{text:?} gave {other:?}"),
        };
        assert_eq!(key("compose = 'db'\nrestart = { attempts = 1 }\n", "db"), "restart");
        assert_eq!(key("compose = 'db'\nidle = { stop = false }\n", "db"), "idle");
        assert_eq!(key("run = 'x'\ngrace_period = '1m'\n", "web"), "grace_period");
    }

    #[test]
    fn a_compose_service_waits_ten_minutes_before_it_stops_unless_configured_otherwise() {
        let config = parsed(
            "[settings]\ngrace_period = '20m'\n\
             [services.db]\ncompose = 'postgres'\n\
             [services.cache]\ncompose = 'redis'\ngrace_period = '30s'\n",
        );
        assert_eq!(config.grace_period(&config.services["db"]), Duration::from_mins(20));
        assert_eq!(config.grace_period(&config.services["cache"]), Duration::from_secs(30));
        assert_eq!(Settings::default().grace_period, Duration::from_mins(10));
    }

    #[test]
    fn a_compose_service_declares_a_driver_and_its_logical_resources_with_their_seeds() {
        let config = parsed(
            "[services.db]\ncompose = 'postgres'\ndriver = 'postgres'\n\
             [services.db.resources.dev]\nname = 'app'\n\
             [services.db.resources.test]\nname = 'app_test'\nseed = 'empty'\n\
             [services.db.resources.copy]\nname = 'app_copy'\nseed = { template = 'app_template' }\n\
             [services.db.resources.filled]\nname = 'app_filled'\nseed = { run = 'make seed', timeout = '2m' }\n\
             [services.db.resources.quick]\nname = 'app_quick'\nseed = { run = 'make seed' }\n",
        );
        let db = &config.services["db"];
        assert_eq!(db.driver.as_deref(), Some("postgres"));
        let seed = |key: &str| db.resources[key].seed.clone();
        assert_eq!(db.resources["dev"].name.as_deref(), Some("app"));
        assert_eq!(seed("dev"), Seed::Empty);
        assert_eq!(seed("test"), Seed::Empty);
        assert_eq!(seed("copy"), Seed::Template("app_template".to_owned()));
        assert_eq!(
            seed("filled"),
            Seed::Command {
                run: "make seed".to_owned(),
                timeout: Duration::from_mins(2)
            }
        );
        assert_eq!(
            seed("quick"),
            Seed::Command {
                run: "make seed".to_owned(),
                timeout: Duration::from_mins(10)
            }
        );
        assert!(db.kind("db").is_ok());
    }

    #[test]
    fn a_seed_is_empty_a_template_or_a_command_and_nothing_else() {
        for seed in [
            "'full'",
            "{ template = 'a', run = 'b' }",
            "{}",
            "{ timeout = '1m' }",
            "{ template = 'a', timeout = '1m' }",
            "{ copy = 'a' }",
        ] {
            let text = format!(
                "[services.db]\ncompose = 'postgres'\ndriver = 'postgres'\n[services.db.resources.dev]\nname = 'app'\nseed = {seed}\n"
            );
            assert!(
                Value::Table(table(&text)).try_into::<Config>().is_err(),
                "seed {seed} was accepted"
            );
        }
    }

    #[test]
    fn a_driver_belongs_to_a_compose_service_that_declares_resources_for_it() {
        let key = |text: &str| match service(text).kind("db") {
            Err(ConfigError::KeyOfOtherKind { key, .. }) => key,
            other => panic!("{text:?} gave {other:?}"),
        };
        assert_eq!(key("run = 'x'\ndriver = 'postgres'\n"), "driver");
        assert_eq!(key("run = 'x'\nresources = { dev = { name = 'app' } }\n"), "resources");
        assert!(matches!(
            service("compose = 'db'\ndriver = 'postgres'\n").kind("db"),
            Err(ConfigError::DriverWithoutResources { service }) if service == "db"
        ));
        assert!(matches!(
            service("compose = 'db'\nresources = { dev = { name = 'app' } }\n").kind("db"),
            Err(ConfigError::ResourcesWithoutDriver { service }) if service == "db"
        ));
    }

    #[test]
    fn a_compose_service_is_logical_with_a_driver_and_shared_without_one() {
        let config = parsed(
            "[services.db]\ncompose = 'postgres'\ndriver = 'postgres'\nresources = { dev = { name = 'app' } }\n\
             [services.mail]\ncompose = 'mailpit'\n",
        );
        assert_eq!(config.isolation(&config.services["db"]), IsolationMode::Logical);
        assert_eq!(config.isolation(&config.services["mail"]), IsolationMode::Shared);
    }

    #[test]
    fn the_isolation_of_a_service_overrides_the_one_of_the_repository_which_overrides_the_default() {
        let config = parsed(
            "[settings]\nisolation = 'shared'\n\
             [services.cache]\ncompose = 'redis'\ndriver = 'redis'\nresources = { main = {} }\n\
             [services.db]\ncompose = 'postgres'\ndriver = 'postgres'\nisolation = 'logical'\n\
             resources = { dev = { name = 'app' } }\n\
             [services.mail]\ncompose = 'mailpit'\n",
        );
        let isolation = |name: &str| config.isolation(&config.services[name]);
        assert_eq!(isolation("cache"), IsolationMode::Shared);
        assert_eq!(isolation("db"), IsolationMode::Logical);
        // A Service without a Driver has nothing to isolate, whatever the Repository asks for.
        assert_eq!(isolation("mail"), IsolationMode::Shared);
        assert_eq!(Settings::default().isolation, None);
    }

    #[test]
    fn the_isolation_is_for_compose_services_and_logical_needs_a_driver() {
        assert!(matches!(
            service("run = 'x'\nisolation = 'shared'\n").kind("web"),
            Err(ConfigError::KeyOfOtherKind { key: "isolation", .. })
        ));
        assert!(matches!(
            service("compose = 'mailpit'\nisolation = 'logical'\n").kind("mail"),
            Err(ConfigError::LogicalWithoutDriver { service }) if service == "mail"
        ));
        assert!(
            service("compose = 'mailpit'\nisolation = 'shared'\n")
                .kind("mail")
                .is_ok()
        );
    }

    #[test]
    fn the_isolated_mode_is_refused_as_not_there_yet_and_any_other_word_as_unknown() {
        for text in [
            "[settings]\nisolation = 'isolated'\n",
            "[services.db]\ncompose = 'x'\nisolation = 'isolated'\n",
        ] {
            let error = Value::Table(table(text)).try_into::<Config>().unwrap_err().to_string();
            assert!(error.contains("Isolated mode is not in v0.1"), "{error}");
        }
        let error = Value::Table(table("[settings]\nisolation = 'soft'\n"))
            .try_into::<Config>()
            .unwrap_err()
            .to_string();
        assert!(error.contains("\"shared\" or \"logical\""), "{error}");
    }

    #[test]
    fn a_resource_name_is_optional_in_the_configuration_and_checked_when_given() {
        let resources =
            |name: &str| format!("compose = 'redis'\ndriver = 'redis'\nresources = {{ main = {{ {name} }} }}\n");
        assert!(service(&resources("")).kind("cache").is_ok());
        assert!(service(&resources("name = 'app'")).kind("cache").is_ok());
        assert!(matches!(
            service(&resources("name = 'App'")).kind("cache"),
            Err(ConfigError::ResourceName { .. })
        ));
    }

    #[test]
    fn a_driver_is_named_by_a_file_name_that_cannot_leave_the_directory_of_drivers() {
        for bad in ["../x", "a/b", "A", "", "a.b", "a b"] {
            let text = format!("compose = 'db'\ndriver = '{bad}'\nresources = {{ dev = {{ name = 'app' }} }}\n");
            assert!(
                matches!(
                    service(&text).kind("db"),
                    Err(ConfigError::DriverName { name, .. }) if name == bad
                ),
                "{bad:?}"
            );
        }
        let ok = "compose = 'db'\ndriver = 'my-db_2'\nresources = { dev = { name = 'app' } }\n";
        assert!(service(ok).kind("db").is_ok());
    }

    #[test]
    fn a_resource_key_and_name_are_lowercase_words_that_are_safe_in_a_shell_command() {
        let name = |text: &str| match service(text).kind("db") {
            Err(ConfigError::ResourceName { resource, name, .. }) => (resource, name),
            other => panic!("{text:?} gave {other:?}"),
        };
        let with = |key: &str, name: &str, seed: &str| {
            format!(
                "compose = 'db'\ndriver = 'd'\nresources = {{ \"{key}\" = {{ name = '{name}', seed = {seed} }} }}\n"
            )
        };
        for bad in ["App", "a b", "a;b", "a-b", "", "app\"", "$(x)", "é"] {
            assert_eq!(
                name(&with("dev", bad, "'empty'")),
                ("dev".to_owned(), bad.to_owned()),
                "{bad:?}"
            );
        }
        assert_eq!(
            name(&with("dev", "app", "{ template = 'a b' }")),
            ("dev".to_owned(), "a b".to_owned())
        );
        assert_eq!(
            name(&with("De v", "app", "'empty'")),
            ("De v".to_owned(), "De v".to_owned())
        );
        assert!(service(&with("dev_2", "app_2", "'empty'")).kind("db").is_ok());
        // The Worktree slug is appended after a double underscore, which names never hold so that 2 Worktrees never
        // share a resource.
        for bad in ["app__x", "app_"] {
            assert_eq!(
                name(&with("dev", bad, "'empty'")),
                ("dev".to_owned(), bad.to_owned()),
                "{bad:?}"
            );
        }
        // A template is an existing resource, written as it is.
        assert!(
            service(&with("dev", "app", "{ template = 'base__2' }"))
                .kind("db")
                .is_ok()
        );
    }

    #[test]
    fn loading_refuses_a_service_that_is_neither_a_process_nor_a_compose_service() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(VERSIONED_FILE),
            "[services.db]\nports = { db = 5432 }\n",
        )
        .unwrap();
        assert!(matches!(
            load(None, dir.path()),
            Err(ConfigError::ServiceKind { service }) if service == "db"
        ));
    }

    fn worktree(text: &str) -> WorktreeConfig {
        Value::Table(table(text)).try_into::<Config>().unwrap().worktree
    }

    #[test]
    fn a_copied_path_stays_inside_the_worktree() {
        let inside = worktree("[worktree]\ncopy = ['.env', 'certs/local.pem', './dir']\n");
        assert!(inside.validate().is_ok());
        for path in ["/etc/passwd", "../secret", "certs/../../secret", "", "."] {
            let outside = WorktreeConfig {
                copy: vec![path.to_owned()],
                setup: Vec::new(),
            };
            assert!(
                matches!(outside.validate(), Err(ConfigError::WorktreeCopyPath { path: bad }) if bad == path),
                "{path:?} was accepted"
            );
        }
    }

    #[test]
    fn the_worktree_section_lists_files_to_copy_and_commands_to_run() {
        let section = worktree("[worktree]\ncopy = ['.env']\nsetup = ['pnpm install', 'pnpm db:migrate']\n");
        assert_eq!(section.copy, [".env"]);
        assert_eq!(section.setup, ["pnpm install", "pnpm db:migrate"]);
        assert_eq!(worktree("").copy, Vec::<String>::new());
    }

    #[test]
    fn a_port_name_declares_its_port_alone_or_with_a_pool() {
        let web = service("run = 'x'\nports = { http = 3000, test = { port = 3001, pool = [3101, 3102] } }\n");
        assert_eq!(web.ports["http"], PortConfig::declared(3000));
        assert_eq!(
            web.ports["test"],
            PortConfig {
                port: 3001,
                pool: vec![3101, 3102]
            }
        );
        let invalid = |text: &str| {
            Value::Table(table(text))
                .try_into::<ServiceConfig>()
                .unwrap_err()
                .to_string()
        };
        assert!(invalid("run = 'x'\nports = { http = { port = 3000, pol = [3100] } }\n").contains("pol"));
        assert!(invalid("run = 'x'\nports = { http = 70000 }\n").contains("70000"));
    }

    #[test]
    fn pools_and_explicit_ports_of_pinned_branches_are_declared_ports() {
        let config = parsed(
            "[services.web]\nrun = 'x'\nports = { http = { port = 3000, pool = [3100] } }\n\
             [pinned.develop]\n\
             [pinned.staging.ports]\nweb = { http = 3200 }\n",
        );
        assert_eq!(config.declared_ports(), BTreeSet::from([3000, 3100, 3200]));
        assert!(config.pinned["develop"].ports.is_empty());
        assert!(config.validate_pinned().is_ok());
    }

    #[test]
    fn an_explicit_port_of_a_pinned_branch_must_name_a_declared_port_name() {
        for pinned in ["api = { http = 3200 }", "web = { test = 3200 }"] {
            let config = parsed(&format!(
                "[services.web]\nrun = 'x'\nports = {{ http = 3000 }}\n[pinned.develop.ports]\n{pinned}\n"
            ));
            assert!(
                matches!(config.validate_pinned(), Err(ConfigError::UnknownPinnedPort { branch, .. }) if branch == "develop"),
                "{pinned} was accepted"
            );
        }
    }

    #[test]
    fn a_lease_expires_after_30_days_unless_configured_otherwise() {
        assert_eq!(Settings::default().lease_expiry, Duration::from_hours(30 * 24));
        assert_eq!(
            parsed("[settings]\nlease_expiry = '7d'\n").settings.lease_expiry,
            Duration::from_hours(7 * 24)
        );
    }

    #[test]
    fn merging_overrides_a_value_and_keeps_its_siblings() {
        let mut base = table("[services.web]\nrun = 'serve'\nports = { http = 3000, test = 3001 }\n");
        merge(&mut base, table("[services.web.ports]\nhttp = 4000\n"));
        assert_eq!(
            base,
            table("[services.web]\nrun = 'serve'\nports = { http = 4000, test = 3001 }\n")
        );
    }

    #[test]
    fn merging_adds_what_the_level_below_lacks() {
        let mut base = table("[services.web]\nrun = 'serve'\n");
        merge(&mut base, table("[services.api]\nrun = 'api'\n"));
        assert_eq!(
            base,
            table("[services.web]\nrun = 'serve'\n[services.api]\nrun = 'api'\n")
        );
    }

    #[test]
    fn merging_replaces_a_table_by_a_value_and_a_value_by_a_table() {
        let mut base = table("a = { b = 1 }\nc = 2\n");
        merge(&mut base, table("a = 3\nc = { d = 4 }\n"));
        assert_eq!(base, table("a = 3\nc = { d = 4 }\n"));
    }

    const MONOREPO: &str = "[services.web]\nrun = 'x'\nports = { http = 3000 }\n\
                            [services.api]\nrun = 'x'\nports = { http = 4000 }\n\
                            [services.dev]\nrun = 'turbo dev'\ncovers = ['web', 'api']\n";

    #[test]
    fn a_process_service_that_covers_services_is_a_group() {
        let config = parsed(MONOREPO);
        assert_eq!(
            config.services["dev"].kind("dev").unwrap(),
            ServiceKind::Group {
                run: "turbo dev",
                covers: &["web".to_owned(), "api".to_owned()]
            }
        );
        assert_eq!(config.group_of("web"), Some("dev"));
        assert_eq!(config.group_of("api"), Some("dev"));
        assert_eq!(config.group_of("dev"), None);
    }

    #[test]
    fn a_group_refuses_the_keys_that_belong_to_the_services_it_covers() {
        let key = |text: &str| match service(text).kind("dev") {
            Err(ConfigError::KeyOfOtherKind { key, .. }) => key,
            other => panic!("{text:?} gave {other:?}"),
        };
        assert_eq!(key("run = 'x'\ncovers = ['web']\nports = { http = 3000 }\n"), "ports");
        assert_eq!(key("run = 'x'\ncovers = ['web']\nenv = { A = 'b' }\n"), "env");
        assert_eq!(key("run = 'x'\ncovers = ['web']\nhealth = { path = '/' }\n"), "health");
        assert_eq!(key("compose = 'db'\ncovers = ['web']\n"), "covers");
        assert!(
            service("run = 'x'\ncovers = ['web']\nhealth = { timeout = '5s' }\n")
                .kind("dev")
                .is_ok()
        );
    }

    #[test]
    fn a_group_covers_at_least_one_service() {
        assert!(matches!(
            service("run = 'x'\ncovers = []\n").kind("dev"),
            Err(ConfigError::EmptyGroup { service }) if service == "dev"
        ));
    }

    #[test]
    fn a_group_covers_declared_process_services_that_no_other_group_covers() {
        let check = |text: &str| parsed(text).validate_groups();
        assert!(check(MONOREPO).is_ok());
        assert!(matches!(
            check("[services.dev]\nrun = 'x'\ncovers = ['web']\n"),
            Err(ConfigError::UnknownCovered { group, service }) if group == "dev" && service == "web"
        ));
        assert!(matches!(
            check("[services.db]\ncompose = 'pg'\n[services.dev]\nrun = 'x'\ncovers = ['db']\n"),
            Err(ConfigError::CoveredNotProcess { group, service }) if group == "dev" && service == "db"
        ));
        assert!(matches!(
            check("[services.web]\nrun = 'x'\ncovers = ['web']\n"),
            Err(ConfigError::CoveredNotProcess { group, service }) if group == "web" && service == "web"
        ));
        assert!(matches!(
            check(&format!("{MONOREPO}[services.all]\nrun = 'x'\ncovers = ['api']\n")),
            Err(ConfigError::CoveredTwice { service, groups }) if service == "api" && groups == ["all", "dev"]
        ));
    }

    #[test]
    fn starting_a_group_starts_what_it_and_the_services_it_covers_need_but_not_the_covered_services() {
        let config = parsed(
            "[services.web]\nrun = 'x'\nneeds = ['api', 'db']\n\
             [services.api]\nrun = 'x'\nneeds = ['db']\n\
             [services.db]\nrun = 'x'\n\
             [services.queue]\nrun = 'x'\n\
             [services.dev]\nrun = 'x'\ncovers = ['web', 'api']\nneeds = ['queue']\n",
        );
        // `api` is covered by the group that `web` is in, so it does not start apart from the group.
        assert_eq!(config.start_order("dev").unwrap(), ["queue", "db", "dev"]);
        assert_eq!(config.start_order("web").unwrap(), ["db", "api", "web"]);
    }

    #[test]
    fn up_without_arguments_starts_every_service_no_group_covers_needs_first() {
        let config = parsed(
            "[services.web]\nrun = 'x'\nneeds = ['api']\n\
             [services.api]\nrun = 'x'\n\
             [services.db]\ncompose = 'pg'\n\
             [services.worker]\nrun = 'x'\nneeds = ['db']\n",
        );
        assert_eq!(config.start_order_all().unwrap(), ["api", "db", "web", "worker"]);

        let grouped = parsed(&format!("{MONOREPO}[services.db]\ncompose = 'pg'\n"));
        assert_eq!(grouped.start_order_all().unwrap(), ["db", "dev"]);
    }

    #[test]
    fn a_need_on_a_covered_service_is_the_group_for_a_service_outside_it() {
        let config = parsed(
            "[services.api]\nrun = 'x'\n\
             [services.web]\nrun = 'x'\n\
             [services.app]\nrun = 'x'\nneeds = ['api']\n\
             [services.dev]\nrun = 'x'\ncovers = ['api', 'web']\n",
        );
        // `api` only runs as `dev`, a Group that cannot start over `api` running alone.
        assert_eq!(config.start_order("app").unwrap(), ["dev", "app"]);
        assert_eq!(config.start_order_all().unwrap(), ["dev", "app"]);
        // A Service of the Group itself still needs the Service alone.
        let inside = parsed(
            "[services.api]\nrun = 'x'\n\
             [services.web]\nrun = 'x'\nneeds = ['api']\n\
             [services.dev]\nrun = 'x'\ncovers = ['api', 'web']\n",
        );
        assert_eq!(inside.start_order("web").unwrap(), ["api", "web"]);
    }

    #[test]
    fn a_group_does_not_need_a_service_it_covers() {
        let config =
            parsed("[services.web]\nrun = 'x'\n[services.dev]\nrun = 'x'\ncovers = ['web']\nneeds = ['web']\n");
        assert!(matches!(
            config.validate_groups(),
            Err(ConfigError::GroupNeedsCovered { group, service }) if group == "dev" && service == "web"
        ));
    }

    #[test]
    fn up_without_arguments_needs_a_declared_service() {
        assert!(matches!(parsed("").start_order_all(), Err(ConfigError::NoServices)));
    }
}

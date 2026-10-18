//! The configuration of a Repository: user settings, the versioned `procly.toml` and the git-excluded
//! `procly.local.toml` of the Worktree, merged in that order.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Deserializer};
use toml::{Table, Value};

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
    #[error("the user settings {path} holds only [settings], move [{key}] to a procly.toml")]
    UserSettingsScope { path: PathBuf, key: String },
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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            health_timeout: default_health_timeout(),
            restart_attempts: default_restart_attempts(),
            restart_delay: default_restart_delay(),
        }
    }
}

/// A Process service.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServiceConfig {
    /// The command, run by `/bin/sh -c` at the root of the Worktree.
    pub(crate) run: String,
    /// The port of each Port name.
    #[serde(default)]
    pub(crate) ports: BTreeMap<String, u16>,
    #[serde(default)]
    pub(crate) health: HealthConfig,
    /// The Services started first, and healthy, before this one.
    #[serde(default)]
    pub(crate) needs: Vec<String>,
    #[serde(default)]
    pub(crate) restart: RestartConfig,
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

impl ServiceConfig {
    /// The port the health check of the Service `name` probes, `None` when it declares no Port name.
    pub(crate) fn health_port(&self, name: &str) -> Result<Option<u16>, ConfigError> {
        match &self.health.port_name {
            Some(port_name) => match self.ports.get(port_name) {
                Some(&port) => Ok(Some(port)),
                None => Err(ConfigError::UnknownHealthPort {
                    service: name.to_owned(),
                    name: port_name.clone(),
                }),
            },
            None if self.ports.len() > 1 => Err(ConfigError::AmbiguousHealthPort {
                service: name.to_owned(),
                names: self.ports.keys().cloned().collect(),
            }),
            None => Ok(self.ports.values().next().copied()),
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

    /// The Services to bring up for `service`, each one after everything it needs and `service` last. A Service
    /// needed through several paths appears once.
    pub(crate) fn start_order(&self, service: &str) -> Result<Vec<String>, ConfigError> {
        let mut order = Vec::new();
        self.visit(service, &mut Vec::new(), &mut order)?;
        Ok(order)
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
            self.visit(need, path, order)?;
        }
        path.pop();
        order.push(name.to_owned());
        Ok(())
    }

    /// How long `up` waits for `service` to become healthy.
    pub(crate) fn health_timeout(&self, service: &ServiceConfig) -> Duration {
        service.health.timeout.unwrap_or(self.settings.health_timeout)
    }
}

/// Long enough for a dev server compiling a large app on its first start.
fn default_health_timeout() -> Duration {
    Duration::from_mins(1)
}

fn default_restart_attempts() -> u32 {
    5
}

fn default_restart_delay() -> Duration {
    Duration::from_secs(1)
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
    Value::Table(merged)
        .try_into()
        .map_err(|source| ConfigError::Invalid { files, source })
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
                .health_port("web")
                .unwrap(),
            Some(3000)
        );
        assert_eq!(service("run = 'x'\n").health_port("worker").unwrap(), None);
    }

    #[test]
    fn the_health_check_probes_the_named_port_among_several() {
        let web = service("run = 'x'\nports = { http = 3000, test = 3001 }\nhealth = { port = 'test' }\n");
        assert_eq!(web.health_port("web").unwrap(), Some(3001));
    }

    #[test]
    fn the_health_port_must_be_named_among_several_and_declared() {
        let ambiguous = service("run = 'x'\nports = { http = 3000, test = 3001 }\n");
        assert!(matches!(
            ambiguous.health_port("web"),
            Err(ConfigError::AmbiguousHealthPort { names, .. }) if names == ["http", "test"]
        ));
        let unknown = service("run = 'x'\nports = { http = 3000 }\nhealth = { port = 'api' }\n");
        assert!(matches!(
            unknown.health_port("web"),
            Err(ConfigError::UnknownHealthPort { name, .. }) if name == "api"
        ));
    }

    fn config(text: &str) -> Config {
        Value::Table(table(text)).try_into().unwrap()
    }

    #[test]
    fn the_needs_of_a_service_start_before_it_and_once_each() {
        let config = config(
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
        let config = config("[services.web]\nrun = 'x'\nneeds = ['db']\n");
        assert!(matches!(
            config.start_order("web"),
            Err(ConfigError::UnknownNeed { service, need }) if service == "web" && need == "db"
        ));
    }

    #[test]
    fn needs_that_loop_back_are_an_error_listing_the_cycle() {
        let config = config(
            "[services.web]\nrun = 'x'\nneeds = ['api']\n\
             [services.api]\nrun = 'x'\nneeds = ['db']\n\
             [services.db]\nrun = 'x'\nneeds = ['api']\n",
        );
        assert!(matches!(
            config.start_order("web"),
            Err(ConfigError::NeedsCycle { cycle }) if cycle == ["api", "db", "api"]
        ));
        let itself = self::config("[services.web]\nrun = 'x'\nneeds = ['web']\n");
        assert!(matches!(
            itself.start_order("web"),
            Err(ConfigError::NeedsCycle { cycle }) if cycle == ["web", "web"]
        ));
    }

    #[test]
    fn a_service_restarts_five_times_from_one_second_unless_configured_otherwise() {
        let config = config("[services.web]\nrun = 'x'\n");
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
        let config = config(
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
}

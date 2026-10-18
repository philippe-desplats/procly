//! The configuration of a Repository: user settings, the versioned `procly.toml` and the git-excluded
//! `procly.local.toml` of the Worktree, merged in that order.

use std::collections::BTreeMap;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

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
    #[error("worktree.copy lists {path:?}, which is not a path inside the Worktree")]
    WorktreeCopyPath { path: String },
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
    #[serde(default)]
    pub(crate) worktree: WorktreeConfig,
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
    /// Where the Leases of Worktrees other than the Main worktree are drawn from.
    #[serde(default = "default_port_range", deserialize_with = "range")]
    pub(crate) port_range: Range,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            health_timeout: default_health_timeout(),
            port_range: default_port_range(),
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

impl ServiceConfig {
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
    /// Every port declared in the configuration, which the Range skips so that it never hands out the port a
    /// Main worktree expects.
    pub(crate) fn declared_ports(&self) -> std::collections::BTreeSet<u16> {
        self.services
            .values()
            .flat_map(|service| service.ports.values().copied())
            .collect()
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

fn default_port_range() -> Range {
    Range::DEFAULT
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

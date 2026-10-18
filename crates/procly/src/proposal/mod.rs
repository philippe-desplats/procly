//! `procly init`: a `procly.toml` proposed from the package scripts and the compose file of a Worktree.
//!
//! The proposal is a starting point to review, not a guess to trust: it declares only what the Worktree states, and
//! leaves out what it would have to invent, such as the port of a dev server that names none.

mod compose;
mod scripts;

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProposalError {
    #[error("cannot read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid JSON in {path}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid YAML in {path}")]
    Yaml {
        path: PathBuf,
        #[source]
        source: serde_yaml_ng::Error,
    },
    #[error("{path} has no `services` table of compose services")]
    NoComposeServices { path: PathBuf },
    #[error("{dir} has no dev script in a package.json and no compose file, so there is nothing to propose")]
    Nothing { dir: PathBuf },
}

/// One `[services.<name>]` table of the proposal. The fields are written in the order they are declared here, the keys
/// before the tables.
#[derive(Debug, Default, Serialize)]
pub(crate) struct ServiceEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) run: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) compose: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) covers: Option<Vec<String>>,
    /// The Driver that gives each Worktree its own Logical resources, a Compose service without one runs Shared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) driver: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) env: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) health: Option<HealthEntry>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) ports: BTreeMap<String, u16>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) resources: BTreeMap<String, ResourceEntry>,
}

/// The `health` table of a Service that declares several Port names.
#[derive(Debug, Serialize)]
pub(crate) struct HealthEntry {
    /// The Port name the health check probes.
    pub(crate) port: String,
}

/// One Logical resource of a Compose service.
#[derive(Debug, Serialize)]
pub(crate) struct ResourceEntry {
    pub(crate) name: String,
}

impl ServiceEntry {
    /// A Process service that runs `run`.
    fn process(run: String) -> Self {
        Self {
            run: Some(run),
            ..Self::default()
        }
    }
}

/// The Services proposed so far, by name.
#[derive(Debug, Default, Serialize)]
struct Proposal {
    services: BTreeMap<String, ServiceEntry>,
}

impl Proposal {
    /// Adds a Service under a name based on `base` that no other Service of the proposal has, and returns it.
    fn add(&mut self, base: &str, entry: ServiceEntry) -> String {
        self.add_with(base, |_| entry)
    }

    /// Adds the Service `entry` builds once it knows its name, which its own templates may need.
    fn add_with(&mut self, base: &str, entry: impl FnOnce(&str) -> ServiceEntry) -> String {
        let taken: BTreeSet<&String> = self.services.keys().collect();
        let name = unique(&taken, base);
        let entry = entry(&name);
        self.services.insert(name.clone(), entry);
        name
    }
}

/// `base`, or `base-2`, `base-3` and so on when the name is taken.
fn unique(taken: &BTreeSet<&String>, base: &str) -> String {
    if !taken.contains(&base.to_owned()) {
        return base.to_owned();
    }
    (2..=u32::MAX)
        .map(|n| format!("{base}-{n}"))
        .find(|candidate| !taken.contains(candidate))
        .expect("an unbounded range always yields a free name")
}

/// A Service name made of what a package or compose name holds that a placeholder can carry: lowercase letters,
/// digits, hyphens and underscores.
fn service_name(raw: &str, fallback: &str) -> String {
    let name: String = raw
        .rsplit('/')
        .next()
        .unwrap_or(raw)
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '-' })
        .collect();
    let name = name.trim_matches('-');
    if name.is_empty() {
        fallback.to_owned()
    } else {
        name.to_owned()
    }
}

/// The `procly.toml` proposed for the Worktree rooted at `worktree`.
pub(crate) fn propose(worktree: &Path) -> Result<String, ProposalError> {
    let mut proposal = Proposal::default();
    scripts::propose(worktree, &mut proposal)?;
    compose::propose(worktree, &mut proposal)?;
    if proposal.services.is_empty() {
        return Err(ProposalError::Nothing {
            dir: worktree.to_owned(),
        });
    }
    let body = toml::to_string(&proposal).expect("a proposal holds only strings, numbers and tables, which serialize");
    Ok(format!(
        "# Proposed by `procly init`. Review it before you commit it, `procly up` starts what it declares.\n\n{body}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_service_name_drops_the_scope_and_keeps_what_a_placeholder_carries() {
        for (raw, name) in [
            ("@acme/web", "web"),
            ("My App", "my-app"),
            ("api_v2", "api_v2"),
            ("web.app", "web-app"),
            ("@acme/", "app"),
            ("---", "app"),
        ] {
            assert_eq!(service_name(raw, "app"), name, "{raw}");
        }
    }

    #[test]
    fn a_taken_name_is_numbered() {
        let mut proposal = Proposal::default();
        assert_eq!(proposal.add("web", ServiceEntry::default()), "web");
        assert_eq!(proposal.add("web", ServiceEntry::default()), "web-2");
        assert_eq!(proposal.add("web", ServiceEntry::default()), "web-3");
    }
}

//! The environment Procly gives a Service, and the files it generates for the tools it does not launch (ADR 0007).
//!
//! The environment of a Service is the one of its caller, then the Leases, the Derived variables, and `PROCLY=1`
//! with the name of the Service. The repository's `.env` is never read nor written, the processes win over it by
//! having the variables already set.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use super::Registry;
use super::lease::{LeaseError, LeaseRequest, PinnedBranch};
use super::managed::{ManagedBlocks, ManagedError, Outcome};
use crate::config::{self, Config, ConfigError, PortConfig, ServiceConfig, ServiceKind};
use crate::derived::{Derived, DerivedError, Facts};
use crate::envfile;
use crate::ports::{self, PortsError};
use crate::topology::{self, Topology, TopologyError};

#[derive(Debug, thiserror::Error)]
pub(super) enum EnvironmentError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Ports(#[from] PortsError),
    #[error(transparent)]
    Derived(#[from] DerivedError),
    #[error(transparent)]
    Lease(#[from] LeaseError),
    #[error(transparent)]
    Managed(#[from] ManagedError),
    #[error(transparent)]
    Topology(#[from] TopologyError),
    #[error("cannot write {path}")]
    WriteEnvFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// The file in the root of a Worktree mirroring the Derived variables, for tools Procly does not launch.
const ENV_FILE: &str = ".env.procly";
/// The name of the Managed block excluding the generated files from git.
const EXCLUSIONS: &str = "exclusions";
/// The shared git exclude file, relative to the common git directory of the Repository.
const EXCLUDE_FILE: &str = "info/exclude";
/// The file `up --gitignore` writes to instead, relative to the root of the Worktree.
const GITIGNORE_FILE: &str = ".gitignore";

/// What a Service receives on top of the environment of its caller.
#[derive(Debug)]
pub(super) struct Injected {
    /// In the order they are set: the Leases, the Derived variables, then `PROCLY` and `PROCLY_SERVICE`.
    pub(super) variables: Vec<(String, String)>,
    /// The Lease of each Port name of the Service.
    pub(super) leases: BTreeMap<String, u16>,
    /// The Lease its health check probes, if it has a Port name.
    pub(super) health_port: Option<u16>,
}

/// Checks the ports of the Service `name` against its health check and its command, and returns the Port name its
/// health check probes. Run before any Lease is granted, so that a request bound to fail stores none.
pub(super) fn check<'a>(name: &str, declared: &'a ServiceConfig) -> Result<Option<&'a str>, EnvironmentError> {
    let health_port_name = declared.health_port_name(name)?;
    // A Compose service has no command, so no placeholder either.
    let command = match declared.kind(name)? {
        ServiceKind::Process { run } => run,
        ServiceKind::Compose { .. } => "",
    };
    ports::check(name, command, declared.ports.keys())?;
    Ok(health_port_name)
}

/// The variables Procly injects into the Service `name`, granting the Leases it and its templates need.
///
/// `starting` tells whether the Service is about to start, which lets the Main worktree take its declared port back.
/// Otherwise a Lease already granted stays as it is, the Service may run on it.
pub(super) fn injected(
    registry: &Registry,
    topology: &Topology,
    config: &Config,
    name: &str,
    declared: &ServiceConfig,
    starting: bool,
) -> Result<Injected, EnvironmentError> {
    let health_port_name = check(name, declared)?;
    let derived = Derived::from_config(&config.services)?;
    let branch = pinned_branch(topology, config)?;

    let mut table = grant_referenced(registry, topology, config, branch, &derived)?;
    let leases = grant(registry, topology, config, branch, name, &declared.ports, !starting)?;
    table.insert(name.to_owned(), leases.clone());
    let health_port = health_port_name.map(|port_name| {
        *leases
            .get(port_name)
            .expect("a Lease is granted for every Port name, the health one included")
    });

    let facts = facts(topology, &table);
    let mut variables = ports::environment(&leases, health_port);
    variables.extend(derived.render(name, &facts)?);
    variables.push(("PROCLY".to_owned(), "1".to_owned()));
    variables.push(("PROCLY_SERVICE".to_owned(), name.to_owned()));
    Ok(Injected {
        variables,
        leases,
        health_port,
    })
}

/// Writes the Env file of the Worktree and makes git ignore it, and `procly.local.toml`, with a Managed block.
///
/// The Env file holds the Derived variables of every Service. A Service-specific variable such as `PORT` has no
/// single value for the Worktree, and `PROCLY` would tell a tool that Procly did not launch that it is supervised.
pub(super) fn mirror(
    registry: &Registry,
    topology: &Topology,
    config: &Config,
    gitignore: bool,
) -> Result<(), EnvironmentError> {
    // One writer at a time, from the rendering to the writing, so that the last one to write also rendered last.
    let _mirroring = registry
        .mirroring
        .lock()
        .expect("the mirroring mutex is never poisoned, no code panics while holding it");
    let derived = Derived::from_config(&config.services)?;
    let table = grant_referenced(registry, topology, config, pinned_branch(topology, config)?, &derived)?;
    let variables = derived.render_all(&facts(topology, &table))?;
    // Excluded first, so that the Env file is never visible to git, not even for a moment.
    exclude(registry, topology, gitignore)?;
    write_env_file(&topology.worktree.path, &envfile::env_file(&variables))
}

fn facts<'a>(topology: &'a Topology, leases: &'a BTreeMap<String, BTreeMap<String, u16>>) -> Facts<'a> {
    Facts {
        slug: &topology.worktree.slug,
        is_main: topology.worktree.main,
        leases,
    }
}

/// The Pinned branch the Worktree is on, as the configuration names it, `None` when its branch is not pinned.
///
/// Only a Repository declaring Pinned branches needs to know the branch. The Main worktree keeps Leases of its own
/// whatever its branch, its declared ports are already fixed, and a Lease of the branch would keep them from it once
/// it checks out another one.
fn pinned_branch<'a>(topology: &Topology, config: &'a Config) -> Result<Option<&'a str>, EnvironmentError> {
    if config.pinned.is_empty() || topology.worktree.main {
        return Ok(None);
    }
    let branch = topology::branch(&topology.worktree.path)?;
    Ok(branch
        .and_then(|branch| config.pinned.get_key_value(&branch))
        .map(|(name, _)| name.as_str()))
}

/// The Leases of every Service a template reads one of, which are bound whether that Service runs or not.
fn grant_referenced(
    registry: &Registry,
    topology: &Topology,
    config: &Config,
    branch: Option<&str>,
    derived: &Derived,
) -> Result<BTreeMap<String, BTreeMap<String, u16>>, EnvironmentError> {
    let mut table = BTreeMap::new();
    for service in derived.referenced() {
        let declared = config
            .services
            .get(service)
            .expect("Derived::from_config checked that every referenced Service is declared");
        table.insert(
            service.to_owned(),
            grant(registry, topology, config, branch, service, &declared.ports, true)?,
        );
    }
    Ok(table)
}

/// Grants the Leases of the Service `service`. The container of a Compose service is shared by every Worktree, so it
/// holds the Leases of the Main worktree, whichever Worktree asks.
fn grant(
    registry: &Registry,
    topology: &Topology,
    config: &Config,
    branch: Option<&str>,
    service: &str,
    ports: &BTreeMap<String, PortConfig>,
    keep: bool,
) -> Result<BTreeMap<String, u16>, EnvironmentError> {
    let shared;
    let compose = config
        .services
        .get(service)
        .is_some_and(|declared| declared.compose.is_some());
    // The container of a Compose service is shared by every Worktree, so it holds the Leases of the Main worktree
    // and never those of a Pinned branch.
    let (topology, pinned) = if compose {
        shared = topology.main()?;
        (&shared, None)
    } else {
        let pinned = branch.map(|name| PinnedBranch {
            name,
            ports: config.pinned.get(name).and_then(|pinned| pinned.ports.get(service)),
        });
        (topology, pinned)
    };
    Ok(registry.leases.grant(&LeaseRequest {
        topology,
        service,
        ports,
        range: config.settings.port_range,
        declared: &config.declared_ports(),
        pinned,
        expiry: config.settings.lease_expiry,
        running: &registry.running(),
        keep,
    })?)
}

/// Replaces the Env file in one step, readable by the user only since a Derived variable may carry a credential,
/// and leaves it alone when it already holds `text`.
fn write_env_file(worktree: &Path, text: &str) -> Result<(), EnvironmentError> {
    let path = worktree.join(ENV_FILE);
    if fs::read_to_string(&path).is_ok_and(|current| current == text) {
        return Ok(());
    }
    let partial = worktree.join(format!("{ENV_FILE}.partial"));
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&partial)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .and_then(|()| fs::rename(&partial, &path))
        .map_err(|source| EnvironmentError::WriteEnvFile { path, source })
}

/// Excludes the generated files from git, in the shared exclude file of the Repository, or in the `.gitignore` of the
/// Worktree on request. A `.gitignore` that already holds the block keeps it, so that a later `up` without the flag
/// does not add the block to the exclude file as well. The reverse is not undone, an earlier block stays in the
/// exclude file.
fn exclude(registry: &Registry, topology: &Topology, gitignore: bool) -> Result<(), EnvironmentError> {
    let gitignore_file = topology.worktree.path.join(GITIGNORE_FILE);
    let file = if gitignore || ManagedBlocks::holds(&gitignore_file, EXCLUSIONS)? {
        gitignore_file
    } else {
        topology.git_common_dir.join(EXCLUDE_FILE)
    };
    let lines = [format!("/{ENV_FILE}"), format!("/{}", config::LOCAL_FILE)];
    if registry.managed.write(&file, EXCLUSIONS, &lines)? == Outcome::Edited {
        eprintln!(
            "the exclusion block in {} was edited, left as it is, check that it still excludes {ENV_FILE}",
            file.display()
        );
    }
    Ok(())
}

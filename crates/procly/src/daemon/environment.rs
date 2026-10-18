//! The environment Procly gives a Service, and the files it generates for the tools it does not launch (ADR 0007).
//!
//! The environment of a Service is the one of its caller, then the Leases, the Derived variables, and `PROCLY=1`
//! with the name of the Service. The repository's `.env` is never read nor written, the processes win over it by
//! having the variables already set.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

use anyhow::Context as _;

use super::Registry;
use super::lease::LeaseRequest;
use super::managed::{ManagedBlocks, Outcome};
use crate::config::{self, Config, ServiceConfig};
use crate::derived::{Derived, Facts};
use crate::envfile;
use crate::ports;
use crate::topology::Topology;

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
pub(super) fn check<'a>(name: &str, declared: &'a ServiceConfig) -> anyhow::Result<Option<&'a str>> {
    let health_port_name = declared.health_port_name(name)?;
    ports::check(name, &declared.run, declared.ports.keys())?;
    Ok(health_port_name)
}

/// The variables Procly injects into the Service `name`, granting the Leases it and its templates need.
pub(super) fn injected(
    registry: &Registry,
    topology: &Topology,
    config: &Config,
    name: &str,
    declared: &ServiceConfig,
) -> anyhow::Result<Injected> {
    let health_port_name = check(name, declared)?;
    let derived = Derived::from_config(&config.services)?;

    let mut table = grant_referenced(registry, topology, config, &derived)?;
    let leases = grant(registry, topology, config, name, &declared.ports)?;
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
pub(super) fn mirror(registry: &Registry, topology: &Topology, config: &Config, gitignore: bool) -> anyhow::Result<()> {
    let derived = Derived::from_config(&config.services)?;
    let table = grant_referenced(registry, topology, config, &derived)?;
    let variables = derived.render_all(&facts(topology, &table))?;
    // One writer at a time, they share the temporary file of the Env file and the exclusion file of the Repository.
    let _mirroring = registry
        .mirroring
        .lock()
        .expect("the mirroring mutex is never poisoned, no code panics while holding it");
    write_env_file(&topology.worktree.path, &envfile::dotenv(&variables))?;
    exclude(registry, topology, gitignore)
}

fn facts<'a>(topology: &'a Topology, leases: &'a BTreeMap<String, BTreeMap<String, u16>>) -> Facts<'a> {
    Facts {
        slug: &topology.worktree.slug,
        is_main: topology.worktree.main,
        leases,
    }
}

/// The Leases of every Service a template reads one of, which are bound whether that Service runs or not.
fn grant_referenced(
    registry: &Registry,
    topology: &Topology,
    config: &Config,
    derived: &Derived,
) -> anyhow::Result<BTreeMap<String, BTreeMap<String, u16>>> {
    let mut table = BTreeMap::new();
    for service in derived.referenced() {
        let declared = config
            .services
            .get(service)
            .expect("Derived::from_config checked that every referenced Service is declared");
        table.insert(
            service.to_owned(),
            grant(registry, topology, config, service, &declared.ports)?,
        );
    }
    Ok(table)
}

fn grant(
    registry: &Registry,
    topology: &Topology,
    config: &Config,
    service: &str,
    ports: &BTreeMap<String, u16>,
) -> anyhow::Result<BTreeMap<String, u16>> {
    Ok(registry.leases.grant(&LeaseRequest {
        topology,
        service,
        ports,
        range: config.settings.port_range,
        declared: &config.declared_ports(),
    })?)
}

/// Replaces the Env file in one step, readable by the user only since a Derived variable may carry a credential,
/// and leaves it alone when it already holds `text`.
fn write_env_file(worktree: &Path, text: &str) -> anyhow::Result<()> {
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
        .with_context(|| format!("cannot write {}", path.display()))
}

/// Excludes the generated files from git, in the shared exclude file of the Repository, or in the `.gitignore` of the
/// Worktree on request. A `.gitignore` that already holds the block keeps it, so that the lines never end up in
/// both files.
fn exclude(registry: &Registry, topology: &Topology, gitignore: bool) -> anyhow::Result<()> {
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

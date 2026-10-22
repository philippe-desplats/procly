//! The environment Procly gives a Service, and the files it generates for the tools it does not launch (ADR 0007).
//!
//! The environment of a Service is the one of its caller, then the Leases, the Derived variables, and `PROCLY=1`
//! with the name of the Service. The repository's `.env` is never read nor written, the processes win over it by
//! having the variables already set.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::Registry;
use super::atomic_file::{self, Mode};
use super::foreign;
use super::health::HealthCheck;
use super::index::{IndexError, IndexRequest};
use super::lease::{Claimed, LeaseError, LeaseRequest, PinnedBranch};
use super::managed::{ManagedBlocks, ManagedError, Outcome};
use super::resource::{LogicalResource, ResourceKey};
use super::store::StoreError;
use crate::config::{self, Config, ConfigError, IsolationMode, PortConfig, ServiceConfig, ServiceKind};
use crate::derived::{Derived, DerivedError, Facts, Resource};
use crate::driver;
use crate::driver::{Driver, DriverError};
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
    Driver(#[from] DriverError),
    #[error(transparent)]
    Lease(#[from] LeaseError),
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error(transparent)]
    Store(#[from] StoreError),
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
pub(super) const ENV_FILE: &str = ".env.procly";
/// The name the fingerprint of an Env file is recorded under, which tells it from one the developer edited.
pub(super) const ENV_BLOCK: &str = "env-file";
/// The name of the Managed block excluding the generated files from git.
pub(super) const EXCLUSIONS: &str = "exclusions";
/// The shared git exclude file, relative to the common git directory of the Repository.
pub(super) const EXCLUDE_FILE: &str = "info/exclude";
/// The file `up --gitignore` writes to instead, relative to the root of the Worktree.
pub(super) const GITIGNORE_FILE: &str = ".gitignore";

/// A Service declared in a Worktree, with what working out its environment takes.
#[derive(Debug, Clone, Copy)]
pub(super) struct DeclaredService<'a> {
    pub(super) registry: &'a Registry,
    pub(super) topology: &'a Topology,
    pub(super) config: &'a Config,
    pub(super) name: &'a str,
    pub(super) declared: &'a ServiceConfig,
}

/// Whether the Leases of a Service may move when its environment is worked out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Leasing {
    /// The Service is about to start, which lets the Main worktree take its declared port back.
    Starting,
    /// A Lease already granted stays as it is, the Service may run on it.
    AsGranted,
}

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
        ServiceKind::Process { run } | ServiceKind::Group { run, .. } => run,
        ServiceKind::Compose { .. } => "",
    };
    ports::check(name, command, declared.ports.keys())?;
    Ok(health_port_name)
}

/// The variables Procly injects into the Service, granting the Leases it and its templates need.
pub(super) fn injected(service: &DeclaredService<'_>, leasing: Leasing) -> Result<Injected, EnvironmentError> {
    let DeclaredService {
        registry,
        topology,
        config,
        name,
        declared,
    } = *service;
    let health_port_name = check(name, declared)?;
    let derived = Derived::from_config(&config.services)?;
    let branch = pinned_branch(topology, config)?;

    let mut table = grant_referenced(registry, topology, config, branch, &derived)?;
    let leases = grant(registry, topology, config, branch, name, &declared.ports, leasing)?;
    table.insert(name.to_owned(), leases.clone());
    let health_port = health_port_name.map(|port_name| {
        *leases
            .get(port_name)
            .expect("a Lease is granted for every Port name, the health one included")
    });

    let resources = referenced_resources(registry, topology, config, &derived, &table)?;
    let facts = facts(topology, &table, &resources);
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

/// What a Group receives on top of the environment of its caller.
#[derive(Debug)]
pub(super) struct InjectedGroup {
    /// In the order they are set: the Leases of each covered Service, their Derived variables, then `PROCLY` and
    /// `PROCLY_SERVICE`.
    pub(super) variables: Vec<(String, String)>,
    /// The port of every Lease of the covered Services.
    pub(super) lease_ports: Vec<u16>,
    /// The health check of each covered Service, which the Group passes once all of them do.
    pub(super) checks: Vec<HealthCheck>,
}

/// The variables Procly injects into the Group `group`, granting the Leases of the Services it covers.
///
/// Each covered Service has `PORT` and `PORT_<NAME>` of its own, which would collide in one environment, so the
/// Group gets `PORT_<SERVICE>_<NAME>` instead, such as `PORT_WEB_HTTP`. Their Derived variables are injected as they
/// are, they are named apart already.
pub(super) fn injected_group(
    group: &DeclaredService<'_>,
    covers: &[String],
    leasing: Leasing,
) -> Result<InjectedGroup, EnvironmentError> {
    let DeclaredService {
        registry,
        topology,
        config,
        name,
        ..
    } = *group;
    let derived = Derived::from_config(&config.services)?;
    let branch = pinned_branch(topology, config)?;
    let mut table = grant_referenced(registry, topology, config, branch, &derived)?;
    let mut variables = Vec::new();
    let mut lease_ports = Vec::new();
    let mut checks = Vec::new();
    for service in covers {
        let declared = config
            .services
            .get(service)
            .expect("the configuration was validated, so a covered Service is declared");
        let health_port_name = check(service, declared)?;
        let leases = grant(registry, topology, config, branch, service, &declared.ports, leasing)?;
        for (port_name, port) in &leases {
            variables.push((
                format!("PORT_{}_{}", identifier(service), port_name.to_ascii_uppercase()),
                port.to_string(),
            ));
            lease_ports.push(*port);
        }
        checks.push(HealthCheck::declared(
            health_port_name.and_then(|port_name| leases.get(port_name).copied()),
            &declared.health,
        ));
        table.insert(service.clone(), leases);
    }
    let resources = referenced_resources(registry, topology, config, &derived, &table)?;
    let facts = facts(topology, &table, &resources);
    for service in covers {
        for variable in derived.render(service, &facts)? {
            // Two covered Services may share a variable, they were checked to give it one template.
            if !variables.contains(&variable) {
                variables.push(variable);
            }
        }
    }
    variables.push(("PROCLY".to_owned(), "1".to_owned()));
    variables.push(("PROCLY_SERVICE".to_owned(), name.to_owned()));
    Ok(InjectedGroup {
        variables,
        lease_ports,
        checks,
    })
}

/// The name of a Service as part of a variable name: upper case, every other character an underscore.
fn identifier(service: &str) -> String {
    service
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
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
    let resources = referenced_resources(registry, topology, config, &derived, &table)?;
    let variables = derived.render_all(&facts(topology, &table, &resources))?;
    // Excluded first, so that the Env file is never visible to git, not even for a moment.
    exclude(registry, topology, gitignore)?;
    let text = envfile::env_file(&variables);
    write_env_file(&topology.worktree.path, &text)?;
    registry
        .managed
        .record(&topology.worktree.path.join(ENV_FILE), ENV_BLOCK, &text, None)?;
    Ok(())
}

fn facts<'a>(
    topology: &'a Topology,
    leases: &'a BTreeMap<String, BTreeMap<String, u16>>,
    resources: &'a BTreeMap<String, BTreeMap<String, Resource>>,
) -> Facts<'a> {
    Facts {
        slug: &topology.worktree.slug,
        is_main: topology.worktree.main,
        leases,
        resources,
    }
}

/// The Logical resources of the Worktree for every Compose service some template reads one of. Only names and URLs are
/// worked out here, creating the resources is for `up` of the Compose service. A Driver that numbers its resources has
/// its indexes handed out and stored here, since the URL holds one.
fn referenced_resources(
    registry: &Registry,
    topology: &Topology,
    config: &Config,
    derived: &Derived,
    leases: &BTreeMap<String, BTreeMap<String, u16>>,
) -> Result<BTreeMap<String, BTreeMap<String, Resource>>, EnvironmentError> {
    let mut table = BTreeMap::new();
    for service in derived.resource_services() {
        let declared = config
            .services
            .get(service)
            .expect("Derived::from_config checked that every referenced Service is declared");
        let service_leases = leases
            .get(service)
            .expect("the Leases of every Service a template refers to are granted before its resources");
        let referenced = DeclaredService {
            registry,
            topology,
            config,
            name: service,
            declared,
        };
        if let Some((_, resources)) = logical_resources(&referenced, service_leases)? {
            table.insert(service.to_owned(), resources);
        }
    }
    Ok(table)
}

/// A Driver and the Logical resources it creates for a Worktree, by key.
pub(super) type DriverResources = (Driver, BTreeMap<String, Resource>);

/// The Driver of the Compose service and its Logical resources in the Worktree, `None` when it has no Driver.
///
/// A Service that is Shared gives every Worktree the resources of the Main worktree, and one that is Isolated gives
/// each Worktree's own container the names and indexes the Main worktree has. A Driver that numbers its resources has
/// its indexes handed out here, and stored, since the URL of a resource holds one.
pub(super) fn logical_resources(
    service: &DeclaredService<'_>,
    leases: &BTreeMap<String, u16>,
) -> Result<Option<DriverResources>, EnvironmentError> {
    let DeclaredService {
        registry,
        topology,
        config,
        name,
        declared,
    } = *service;
    let Some(driver) = &declared.driver else {
        return Ok(None);
    };
    let worktree = &topology.worktree;
    let driver = Driver::load(driver, &worktree.path, driver::user_dir().as_deref())?;
    let health_port = declared
        .health_port_name(name)?
        .and_then(|port_name| leases.get(port_name).copied());
    // Whose names the Worktree reads: its own in a Logical container, or the ones of the Main worktree in a container
    // every Worktree shares or in a container of its own.
    let shared = config.isolation(declared) != IsolationMode::Logical;
    let numbered = match driver.index_capacity() {
        Some(capacity) => {
            // Before any index is handed out, so that a configuration bound to fail never takes one.
            if let Some(key) = declared
                .resources
                .iter()
                .find(|(_, resource)| resource.name.is_some())
                .map(|(key, _)| key)
            {
                return Err(not_used(&driver, name, key).into());
            }
            let main = topology.main()?.worktree.path;
            let resources: Vec<String> = declared.resources.keys().cloned().collect();
            let indexes = registry.indexes.allocate(&IndexRequest {
                repository: &topology.git_common_dir,
                main: &main,
                worktree: if shared { &main } else { &worktree.path },
                service: name,
                driver: &driver.name,
                capacity,
                resources: &resources,
            })?;
            Some(indexes)
        }
        None => None,
    };
    let mut resources = BTreeMap::new();
    for (key, resource) in &declared.resources {
        let resource_name = match (&numbered, &resource.name) {
            (Some(indexes), _) => indexes
                .get(key)
                .expect("an index is handed out for every resource asked for")
                .to_string(),
            (None, Some(declared_name)) => {
                driver.resource_name(declared_name, &worktree.slug, worktree.main || shared)?
            }
            (None, None) => return Err(required(&driver, name, key).into()),
        };
        let url = driver.url(&resource_name, leases, health_port)?;
        resources.insert(
            key.clone(),
            Resource {
                name: resource_name,
                url,
            },
        );
    }
    // Only a Logical container holds resources of the Worktree's own, which outlive it until `gc` drops them.
    if !shared && !worktree.main {
        let branch = pinned_branch(topology, config)?.map(str::to_owned);
        registry
            .resources
            .record(resources.iter().map(|(key, resource)| LogicalResource {
                key: ResourceKey {
                    repository: topology.git_common_dir.clone(),
                    worktree: worktree.path.clone(),
                    service: name.to_owned(),
                    resource: key.clone(),
                },
                name: resource.name.clone(),
                driver: driver.name.clone(),
                indexed: numbered.is_some(),
                branch: branch.clone(),
            }))?;
    }
    Ok(Some((driver, resources)))
}

/// The error of a resource of a Driver that names its resources and was given no name.
fn required(driver: &Driver, service: &str, resource: &str) -> DriverError {
    DriverError::NameRequired {
        driver: driver.name.clone(),
        service: service.to_owned(),
        resource: resource.to_owned(),
    }
}

/// The error of a resource given a name by a Driver that numbers its resources.
fn not_used(driver: &Driver, service: &str, resource: &str) -> DriverError {
    DriverError::NameNotUsed {
        driver: driver.name.clone(),
        service: service.to_owned(),
        resource: resource.to_owned(),
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
            grant(
                registry,
                topology,
                config,
                branch,
                service,
                &declared.ports,
                Leasing::AsGranted,
            )?,
        );
    }
    Ok(table)
}

/// Grants the Leases of the Service `service`. The container of a Compose service is shared by every Worktree unless
/// it is Isolated, so it holds the Leases of the Main worktree, whichever Worktree asks. An Isolated container belongs
/// to its Worktree, and holds the Leases of that Worktree as a Process service would.
fn grant(
    registry: &Registry,
    topology: &Topology,
    config: &Config,
    branch: Option<&str>,
    service: &str,
    ports: &BTreeMap<String, PortConfig>,
    leasing: Leasing,
) -> Result<BTreeMap<String, u16>, EnvironmentError> {
    let shared;
    let declared = config.services.get(service);
    let compose = declared.and_then(|declared| declared.compose.as_deref());
    let isolated = declared.is_some_and(|declared| config.isolation(declared) == IsolationMode::Isolated);
    // The container of a Compose service is shared by every Worktree, so it holds the Leases of the Main worktree
    // and never those of a Pinned branch.
    let (topology, pinned) = if compose.is_some() && !isolated {
        shared = topology.main()?;
        (&shared, None)
    } else {
        let pinned = branch.map(|name| PinnedBranch {
            name,
            ports: config.pinned.get(name).and_then(|pinned| pinned.ports.get(service)),
        });
        (topology, pinned)
    };
    // Where the container runs from: the Main worktree, or the Worktree itself when it is Isolated.
    let project_dir = &topology.worktree.path;
    let held_by_container =
        |port: u16| compose.is_some_and(|compose| foreign::published_by_service(port, project_dir, compose));
    Ok(registry.leases.grant(&LeaseRequest {
        topology,
        service,
        ports,
        range: config.settings.port_range,
        declared: &config.declared_ports(),
        pinned,
        expiry: config.settings.lease_expiry,
        running: &registry.running(),
        keep: leasing == Leasing::AsGranted,
        claimed: compose.map(|_| Claimed(&held_by_container)),
    })?)
}

/// Replaces the Env file in one step, readable by the user only since a Derived variable may carry a credential,
/// and leaves it alone when it already holds `text`.
fn write_env_file(worktree: &Path, text: &str) -> Result<(), EnvironmentError> {
    let path = worktree.join(ENV_FILE);
    if fs::read_to_string(&path).is_ok_and(|current| current == text) {
        return Ok(());
    }
    atomic_file::replace(&path, text.as_bytes(), Mode::Private)
        .map_err(|source| EnvironmentError::WriteEnvFile { path, source })
}

/// The lines of the Managed block excluding the generated files from git.
pub(super) fn exclusion_lines() -> [String; 2] {
    [format!("/{ENV_FILE}"), format!("/{}", config::LOCAL_FILE)]
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
    if registry.managed.write(&file, EXCLUSIONS, &exclusion_lines())? == Outcome::Edited {
        eprintln!(
            "the exclusion block in {} was edited, left as it is, check that it still excludes {ENV_FILE}",
            file.display()
        );
    }
    Ok(())
}

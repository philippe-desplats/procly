//! `up`: the Services a request brings up, in the order their needs give, and what each one is started with.
//!
//! The configuration of the Worktree is read once per request into a [`Scope`], so that every step of one `up` sees
//! the same declarations.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::compose::{ComposeError, ComposeSpec, Isolated, PlannedResource, PublishedLease, ResourcePlan};
use super::environment::{self, DeclaredService, Leasing};
use super::health::HealthCheck;
use super::service::{Relation, Service, ServiceSpec, Start};
use super::{Registry, blocking, message, read_config, topology_of};
use crate::config::{Config, IsolationMode, ServiceConfig, ServiceKind};
use crate::driver::{self, Driver};
use crate::ports;
use crate::protocol::{Response, ServiceEnd, StartOutcome, UpService};
use crate::topology::Topology;

/// What `failed` says when a blocking step of `up` could not run to its end.
const START_FAILED: &str = "the service could not be started";

/// The Port names that are the scheme of the protocol their port speaks, such as the `smtp` and `http` ports `procly
/// init` gives a Mailpit.
const SCHEMES: [&str; 5] = ["amqp", "http", "https", "redis", "smtp"];

/// The Worktree a request is about and its configuration, read once for the whole request.
#[derive(Debug, Clone)]
pub(super) struct Scope {
    topology: Topology,
    config: Arc<Config>,
}

impl Scope {
    /// The Worktree holding `cwd` and its configuration. Blocking, it runs git and reads the configuration.
    pub(super) fn of(cwd: &Path) -> anyhow::Result<Self> {
        let topology = topology_of(cwd)?;
        let config = Arc::new(read_config(&topology.worktree.path)?);
        Ok(Self { topology, config })
    }

    pub(super) fn config(&self) -> &Config {
        &self.config
    }

    fn worktree(&self) -> &Path {
        &self.topology.worktree.path
    }

    /// The declaration of the Service `name`, which must be declared.
    fn declared(&self, name: &str) -> anyhow::Result<&ServiceConfig> {
        super::declared(&self.config, self.worktree(), name)
    }

    /// The Service `name`, as its environment is worked out from.
    fn service<'a>(&'a self, registry: &'a Registry, name: &'a str) -> anyhow::Result<DeclaredService<'a>> {
        Ok(DeclaredService {
            registry,
            topology: &self.topology,
            config: &self.config,
            name,
            declared: self.declared(name)?,
        })
    }

    /// Whether the Compose service `name`, which must be declared, runs one container per Worktree.
    fn is_isolated(&self, name: &str) -> bool {
        self.config
            .services
            .get(name)
            .is_some_and(|declared| self.config.isolation(declared) == IsolationMode::Isolated)
    }

    /// Where the container of the Compose service `name` reads its compose file from, and where an Isolated one takes
    /// its project name from. Blocking, finding the Main worktree runs git.
    pub(super) fn project(&self, name: &str) -> anyhow::Result<(PathBuf, Option<Isolated>)> {
        let main = self.topology.main()?.worktree.path;
        if !self.is_isolated(name) {
            return Ok((main, None));
        }
        let worktree = &self.topology.worktree;
        let slug = (!worktree.main).then(|| worktree.slug.clone());
        let pinned = environment::pinned_branch(&self.topology, &self.config)?.map(str::to_owned);
        Ok((worktree.path.clone(), Some(Isolated { main, slug, pinned })))
    }

    /// The Services to bring up for `name`, needs first, or for every Service no Group covers without `name`.
    fn start_order(&self, name: Option<&str>) -> anyhow::Result<Vec<String>> {
        match name {
            Some(name) => {
                self.declared(name)?;
                Ok(self.config.start_order(name)?)
            }
            None => Ok(self.config.start_order_all()?),
        }
    }
}

/// Brings up the Services `name` needs, each one healthy before the next starts, then `name`, and answers with
/// where to reach it. Without `name`, brings up every Service no Group covers. Stops at the first Service that does
/// not become healthy.
pub(super) async fn up(
    registry: &Arc<Registry>,
    name: Option<String>,
    cwd: PathBuf,
    env: Vec<(String, String)>,
    gitignore: bool,
) -> Response {
    let planned = {
        let name = name.clone();
        // Resolving the Topology runs git, and reading the configuration is blocking.
        blocking(START_FAILED, move || {
            let scope = Scope::of(&cwd)?;
            let order = scope.start_order(name.as_deref())?;
            Ok((scope, order))
        })
        .await
    };
    let (scope, order) = match planned {
        Ok(planned) => planned,
        Err(message) => return Response::Error { message },
    };
    let mut reached = Vec::new();
    for current in &order {
        let (outcome, via, port, health) = match start(registry, &scope, current, &env, gitignore).await {
            Ok(Running::Process { service, outcome }) => {
                let health = service.healthy().await.map_err(|end| (end, service.output()));
                (outcome, None, service.port(), health)
            }
            Ok(Running::Covered { group, name, port }) => {
                let health = group.healthy().await.map_err(|end| (end, group.output()));
                (StartOutcome::Reused, Some(name), port, health)
            }
            Ok(Running::Compose { outcome, port, health }) => (outcome, None, port, health),
            Err(response) => {
                return match *response {
                    Response::Conflict { service, conflicts, .. } => Response::Conflict {
                        service,
                        conflicts,
                        needed_by: name.as_ref().filter(|target| *target != current).cloned(),
                    },
                    other => other,
                };
            }
        };
        if let Err((end, output)) = health {
            return Response::Unhealthy {
                service: current.clone(),
                outcome,
                end,
                output,
                needed_by: name.as_ref().filter(|target| *target != current).cloned(),
            };
        }
        let reach = {
            let (registry, scope, current) = (Arc::clone(registry), scope.clone(), current.clone());
            // Reading the Leases and the Driver is blocking.
            blocking(START_FAILED, move || service_leases(&registry, &scope, &current)).await
        };
        let (ports, urls) = match reach {
            Ok(reach) => reach,
            Err(message) => return Response::Error { message },
        };
        // The URL of the Lease the health check probes, among the URLs of every Lease.
        let url = port
            .and_then(|port| ports.iter().find(|&(_, &lease)| lease == port))
            .and_then(|(port_name, _)| urls.get(port_name).cloned());
        reached.push(UpService {
            service: current.clone(),
            outcome,
            via,
            port,
            url,
            ports,
            urls,
        });
    }
    // The Service that was asked for comes last, after everything it needs.
    match (name, reached.pop()) {
        (Some(_), Some(target)) => Response::Up {
            service: target.service,
            outcome: target.outcome,
            via: target.via,
            port: target.port,
            url: target.url,
            ports: target.ports,
            needs: reached,
            urls: target.urls,
        },
        (None, Some(last)) => {
            reached.push(last);
            Response::UpAll { services: reached }
        }
        (name, None) => Response::Error {
            message: format!("no service {} to start", name.unwrap_or_default()),
        },
    }
}

/// The variables Procly injects into the Service `name` of the Worktree holding `cwd`. Blocking.
pub(super) fn service_environment(
    registry: &Registry,
    name: &str,
    cwd: &Path,
) -> anyhow::Result<Vec<(String, String)>> {
    let scope = Scope::of(cwd)?;
    let service = scope.service(registry, name)?;
    let mut variables = match service.declared.kind(name)? {
        ServiceKind::Group { covers, .. } => {
            environment::check(name, service.declared)?;
            environment::injected_group(&service, covers, Leasing::AsGranted)?.variables
        }
        ServiceKind::Process { .. } | ServiceKind::Compose { .. } => {
            environment::injected(&service, Leasing::AsGranted)?.variables
        }
    };
    variables.sort();
    Ok(variables)
}

/// A Service that was started or found running.
enum Running {
    /// A Process service, whose health is still to be waited for.
    Process {
        service: Arc<Service>,
        outcome: StartOutcome,
    },
    /// A Service covered by a Group that runs, whose health is the Group's, still to be waited for.
    Covered {
        group: Arc<Service>,
        /// The name of the Group.
        name: String,
        /// The Lease its health check probes.
        port: Option<u16>,
    },
    /// A Compose service, already waited for: `health` tells whether its container answered in time, and if not,
    /// how it ended and its recent logs.
    Compose {
        outcome: StartOutcome,
        port: Option<u16>,
        health: Result<(), (ServiceEnd, String)>,
    },
}

/// Starts the Service `name`, or finds it running. A Service being stopped still holds its port, so this waits
/// for its tree to be gone and asks again.
async fn start(
    registry: &Arc<Registry>,
    scope: &Scope,
    name: &str,
    env: &[(String, String)],
    gitignore: bool,
) -> Result<Running, Box<Response>> {
    loop {
        let started = {
            let (registry, scope, name, env) = (Arc::clone(registry), scope.clone(), name.to_owned(), env.to_owned());
            // Writing the generated files, granting the Leases and forking are blocking.
            blocking(START_FAILED, move || {
                start_service(&registry, &scope, &name, env, gitignore)
            })
            .await
        };
        match started.map_err(|message| Box::new(Response::Error { message }))? {
            Started::Process(Start::Ready { service, outcome, .. }) => {
                return Ok(Running::Process { service, outcome });
            }
            Started::Covered { group, name, port } => return Ok(Running::Covered { group, name, port }),
            Started::Process(Start::Stopping(service)) => service.ended().await,
            Started::Compose { grace } => return up_compose(registry, scope, name, grace, env.to_owned()).await,
        }
    }
}

/// What a start request found out about the Service it asked for.
enum Started {
    Process(Start),
    /// A Service covered by a Group that runs, which nothing was started for.
    Covered {
        group: Arc<Service>,
        /// The name of the Group.
        name: String,
        /// The Lease its health check probes, since the Group has none of its own.
        port: Option<u16>,
    },
    /// A Compose service, whose container is started by [`up_compose`].
    Compose {
        grace: Duration,
    },
}

fn start_service(
    registry: &Arc<Registry>,
    scope: &Scope,
    name: &str,
    env: Vec<(String, String)>,
    gitignore: bool,
) -> anyhow::Result<Started> {
    let worktree = scope.worktree().to_owned();
    let declared = scope.declared(name)?;
    // Also on a Reuse, so that `--gitignore` is honored whenever it is asked. A Service whose ports do not check out
    // fails here, before any file is written or Lease granted.
    environment::check(name, declared)?;
    environment::mirror(registry, &scope.topology, &scope.config, gitignore)?;
    // The Services the Service covers when it is a Group, or the Group covering it.
    let relation = match declared.kind(name)? {
        ServiceKind::Compose { .. } => {
            return Ok(Started::Compose {
                grace: scope.config.grace_period(declared),
            });
        }
        ServiceKind::Group { covers, .. } => Relation::Group { covers },
        ServiceKind::Process { .. } => match scope.config.group_of(name) {
            Some(by) => Relation::Covered { by },
            None => Relation::Alone,
        },
    };
    // Dated again once it ends, so that a Service which ran longer than the expiry keeps its Leases. A Group holds
    // the Leases of the Services it covers.
    let touched = match relation {
        Relation::Group { covers } => covers.to_vec(),
        Relation::Covered { .. } | Relation::Alone => vec![name.to_owned()],
    };
    let start = registry
        .services
        .start::<anyhow::Error>(&worktree, name, relation, || service_spec(registry, scope, name, env))?;
    match start {
        Start::Ready {
            service,
            outcome: StartOutcome::Reused,
            via: Some(group),
        } => {
            // The Group has no Lease of its own, the covered Service has one the caller wants to hear of.
            let port = environment::injected(&scope.service(registry, name)?, Leasing::AsGranted)?.health_port;
            return Ok(Started::Covered {
                group: service,
                name: group,
                port,
            });
        }
        Start::Ready {
            ref service,
            outcome: StartOutcome::Started,
            ..
        } => {
            let (registry, service) = (Arc::clone(registry), Arc::clone(service));
            tokio::spawn(async move {
                service.ended().await;
                let touched = tokio::task::spawn_blocking(move || {
                    touched
                        .iter()
                        .try_for_each(|name| registry.leases.touch(&worktree, name))
                })
                .await;
                if let Ok(Err(error)) = touched {
                    eprintln!("cannot date the Leases of a service that ended: {error:#}");
                }
            });
        }
        Start::Ready { .. } | Start::Stopping(_) => {}
    }
    Ok(Started::Process(start))
}

/// What the Process service or the Group `name` runs: its command, its environment on top of the one of the caller,
/// and how it is checked. Run under the start mutex, so that 2 Worktrees starting at once never get the same port.
fn service_spec(
    registry: &Registry,
    scope: &Scope,
    name: &str,
    env: Vec<(String, String)>,
) -> anyhow::Result<ServiceSpec> {
    let service = scope.service(registry, name)?;
    let declared = service.declared;
    let (command, variables, health, lease_ports, covers) = match declared.kind(name)? {
        ServiceKind::Process { run } => {
            let injected = environment::injected(&service, Leasing::Starting)?;
            let health = HealthCheck::declared(injected.health_port, &declared.health);
            let lease_ports = injected.leases.values().copied().collect();
            (
                ports::render(run, &injected.leases),
                injected.variables,
                health,
                lease_ports,
                Vec::new(),
            )
        }
        ServiceKind::Group { run, covers } => {
            let injected = environment::injected_group(&service, covers, Leasing::Starting)?;
            // Healthy once every Service it covers is.
            let health = HealthCheck {
                port: None,
                http: None,
                covered: injected.checks,
            };
            (
                run.to_owned(),
                injected.variables,
                health,
                injected.lease_ports,
                covers.to_vec(),
            )
        }
        ServiceKind::Compose { .. } => {
            unreachable!("a Compose service is brought up by up_compose, start_service never asks for its spec")
        }
    };
    // After the environment of the caller, so that a `PORT` of its own never wins over the Lease.
    let mut env = env;
    env.extend(variables);
    let worktree = scope.worktree().to_owned();
    Ok(ServiceSpec {
        command,
        cwd: worktree.clone(),
        env,
        health,
        lease_ports,
        covers,
        health_timeout: scope.config.health_timeout(declared),
        name: name.to_owned(),
        worktree,
        topology: scope.topology.clone(),
        restart: scope.config.restart_policy(declared),
        idle: scope.config.idle_timeout(declared),
    })
}

/// Starts the container of the Compose service `name`, or finds it running for another Worktree, and waits until it
/// is healthy.
async fn up_compose(
    registry: &Arc<Registry>,
    scope: &Scope,
    name: &str,
    grace: Duration,
    env: Vec<(String, String)>,
) -> Result<Running, Box<Response>> {
    let worktree = scope.worktree().to_owned();
    let isolated = scope.is_isolated(name);
    // An Isolated container belongs to its Worktree alone, nobody is left to wait for once it lets go.
    let (owner, grace) = if isolated {
        (Some(worktree.clone()), Duration::ZERO)
    } else {
        (None, grace)
    };
    let key = (scope.topology.git_common_dir.clone(), name.to_owned(), owner);
    let prepare = {
        let (registry, scope, name, env) = (Arc::clone(registry), scope.clone(), name.to_owned(), env.clone());
        move || prepare_compose(&registry, &scope, &name, env)
    };
    let plan = {
        let (registry, scope, name) = (Arc::clone(registry), scope.clone(), name.to_owned());
        move || plan_resources(&registry, &scope, &name, env)
    };
    match registry
        .composes
        .up::<anyhow::Error, _, _>(key, worktree, grace, prepare, plan)
        .await
    {
        Ok(brought) => Ok(Running::Compose {
            outcome: brought.outcome,
            port: brought.port,
            health: brought.health,
        }),
        Err(error) => match error.downcast::<ComposeError>() {
            Ok(ComposeError::Conflict { service, conflicts }) => Err(Box::new(Response::Conflict {
                service,
                conflicts,
                needed_by: None,
            })),
            Ok(error) => Err(Box::new(Response::Error {
                message: message(error),
            })),
            Err(error) => Err(Box::new(Response::Error {
                message: message(error),
            })),
        },
    }
}

/// What it takes to start the container of the Compose service `name`. Run only when the container does not run,
/// so that the Leases of the Main worktree are granted anew then and never under a container using them.
fn prepare_compose(
    registry: &Registry,
    scope: &Scope,
    name: &str,
    env: Vec<(String, String)>,
) -> anyhow::Result<ComposeSpec> {
    let service = scope.service(registry, name)?;
    let declared = service.declared;
    let compose = match declared.kind(name)? {
        ServiceKind::Compose { service } => service,
        ServiceKind::Process { .. } | ServiceKind::Group { .. } => {
            unreachable!("only a Compose service is brought up by up_compose")
        }
    };
    // Granted under the lock of the container, so that 2 Worktrees starting at once never get different ports.
    let injected = environment::injected(&service, Leasing::Starting)?;
    let published = injected
        .leases
        .iter()
        .map(|(port_name, &port)| PublishedLease {
            port_name: port_name.clone(),
            port,
        })
        .collect();
    // What the compose file may interpolate a Lease from: `PORT_<NAME>`, and the Derived variables of the Service.
    let variables = injected
        .leases
        .keys()
        .map(|port_name| format!("PORT_{}", port_name.to_ascii_uppercase()))
        .chain(declared.env.keys().cloned())
        .collect();
    let injected_names = injected.variables.iter().map(|(name, _)| name.clone()).collect();
    let mut env = env;
    env.extend(injected.variables);
    let (project_dir, isolated) = scope.project(name)?;
    Ok(ComposeSpec {
        name: name.to_owned(),
        compose: compose.to_owned(),
        project_dir,
        isolated,
        env,
        published,
        variables,
        injected: injected_names,
        health: HealthCheck::declared(injected.health_port, &declared.health),
        health_timeout: scope.config.health_timeout(declared),
    })
}

/// The Logical resources the Worktree needs in the container of the Compose service `name`, `None` when the service
/// has no Driver. Worked out on every `up`, a Reuse included, since each Worktree has resources of its own.
fn plan_resources(
    registry: &Arc<Registry>,
    scope: &Scope,
    name: &str,
    env: Vec<(String, String)>,
) -> anyhow::Result<Option<ResourcePlan>> {
    let service = scope.service(registry, name)?;
    let declared = service.declared;
    if declared.driver.is_none() {
        return Ok(None);
    }
    // The Leases are the ones the container runs on, granted already, so they stay as they are.
    let injected = environment::injected(&service, Leasing::AsGranted)?;
    let Some((driver, resources)) = environment::logical_resources(&service, &injected.leases)? else {
        return Ok(None);
    };
    let wanted = resources
        .into_iter()
        .map(|(key, resource)| PlannedResource {
            seed: declared.resources[&key].seed.clone(),
            key,
            resource,
        })
        .collect();
    let mut env = env;
    env.extend(injected.variables);
    Ok(Some(ResourcePlan {
        driver,
        topology: scope.topology.clone(),
        resources: wanted,
        env,
        jobs: Arc::clone(&registry.jobs),
        ready_timeout: scope.config.health_timeout(declared),
        command_timeout: scope.config.settings.driver_timeout,
    }))
}

/// The Leases of the Service `name`, by Port name, as `env` exports them, and the URLs of the ones whose scheme is
/// known. A Group has none of its own, the Services it covers have.
fn service_leases(
    registry: &Registry,
    scope: &Scope,
    name: &str,
) -> anyhow::Result<(BTreeMap<String, u16>, BTreeMap<String, String>)> {
    let service = scope.service(registry, name)?;
    let declared = service.declared;
    let leases = match declared.kind(name)? {
        ServiceKind::Group { .. } => return Ok((BTreeMap::new(), BTreeMap::new())),
        ServiceKind::Process { .. } | ServiceKind::Compose { .. } => {
            environment::injected(&service, Leasing::AsGranted)?.leases
        }
    };
    let driver = scheme_driver(name, declared, scope.worktree(), driver::user_dir().as_deref(), || {
        Ok(scope.topology.main()?.worktree.path)
    })?;
    let urls = lease_urls(name, declared, driver.as_ref(), &leases)?;
    Ok((leases, urls))
}

/// The Driver whose URL gives the scheme of the Leases of the Service `name`: the one it declares, else, for a
/// Compose service, the one that ships with Procly for the image the compose file of the Main worktree `main` gives
/// it, so that a Postgres is reached as one whether or not `procly init` gave it a Driver. A Driver found from the
/// image and failing to load leaves the Leases without a scheme rather than failing `up`.
fn scheme_driver(
    name: &str,
    declared: &ServiceConfig,
    worktree: &Path,
    user_dir: Option<&Path>,
    main: impl FnOnce() -> anyhow::Result<PathBuf>,
) -> anyhow::Result<Option<Driver>> {
    if let Some(driver) = declared.driver.as_deref() {
        return Ok(Some(Driver::load(driver, worktree, user_dir)?));
    }
    let ServiceKind::Compose { service } = declared.kind(name)? else {
        return Ok(None);
    };
    Ok(crate::proposal::bundled_driver(&main()?, service)
        .and_then(|driver| Driver::load(driver, worktree, user_dir).ok()))
}

/// Where to reach each of the `leases` of the Service `name`, by Port name, for the Leases whose scheme is known.
///
/// The scheme is the one of the URL of its `driver` for the Lease that URL reaches, else the Port name itself when it
/// is one of [`SCHEMES`], else `http` for the Lease the health check of a Process service probes or the Lease of an
/// HTTP health check. Any other Lease has no URL, rather than an `http` URL for a port that may not speak HTTP.
fn lease_urls(
    name: &str,
    declared: &ServiceConfig,
    driver: Option<&Driver>,
    leases: &BTreeMap<String, u16>,
) -> anyhow::Result<BTreeMap<String, String>> {
    let health_port_name = declared.health_port_name(name)?;
    let health_is_http = match declared.kind(name)? {
        ServiceKind::Process { .. } => true,
        ServiceKind::Compose { .. } | ServiceKind::Group { .. } => declared.health.http().is_some(),
    };
    Ok(leases
        .iter()
        .filter_map(|(port_name, port)| {
            let health = health_port_name == Some(port_name.as_str());
            let scheme = driver
                .and_then(|driver| driver.scheme_of(port_name, health))
                .or_else(|| SCHEMES.iter().copied().find(|scheme| scheme == port_name))
                .or((health && health_is_http).then_some("http"))?;
            Some((port_name.clone(), format!("{scheme}://localhost:{port}")))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared(text: &str) -> ServiceConfig {
        toml::from_str(text).unwrap()
    }

    fn leases(leases: &[(&str, u16)]) -> BTreeMap<String, u16> {
        leases
            .iter()
            .map(|&(port_name, port)| (port_name.to_owned(), port))
            .collect()
    }

    fn urls(urls: &[(&str, &str)]) -> BTreeMap<String, String> {
        urls.iter()
            .map(|&(port_name, url)| (port_name.to_owned(), url.to_owned()))
            .collect()
    }

    #[test]
    fn a_mailpit_gets_an_http_url_for_its_web_port_and_an_smtp_url_for_its_smtp_port() {
        let mailpit =
            declared("compose = 'mailpit'\nhealth = { port = 'smtp' }\nports = { http = 8025, smtp = 1025 }\n");
        assert_eq!(
            lease_urls("mailpit", &mailpit, None, &leases(&[("http", 24000), ("smtp", 24001)])).unwrap(),
            urls(&[("http", "http://localhost:24000"), ("smtp", "smtp://localhost:24001")])
        );
    }

    #[test]
    fn the_health_port_of_a_process_service_is_reached_over_http_and_its_other_ports_by_their_name() {
        let web = declared("run = 'x'\nports = { web = 3000 }\n");
        assert_eq!(
            lease_urls("web", &web, None, &leases(&[("web", 24000)])).unwrap(),
            urls(&[("web", "http://localhost:24000")])
        );
        let multi = declared("run = 'x'\nports = { http = 3000, admin = 3001 }\nhealth = { port = 'http' }\n");
        assert_eq!(
            lease_urls("multi", &multi, None, &leases(&[("http", 24000), ("admin", 24001)])).unwrap(),
            urls(&[("http", "http://localhost:24000")])
        );
    }

    #[test]
    fn a_port_name_that_is_a_scheme_wins_over_the_http_of_a_health_port() {
        let secure = declared("run = 'x'\nports = { https = 8443 }\n");
        assert_eq!(
            lease_urls("secure", &secure, None, &leases(&[("https", 24000)])).unwrap(),
            urls(&[("https", "https://localhost:24000")])
        );
    }

    #[test]
    fn a_compose_port_is_reached_over_http_only_when_its_health_check_is_http() {
        let db = declared("compose = 'db'\nports = { db = 5432 }\n");
        assert_eq!(
            lease_urls("db", &db, None, &leases(&[("db", 24000)])).unwrap(),
            urls(&[])
        );
        let console = declared("compose = 'minio'\nports = { console = 9001 }\nhealth = { path = '/' }\n");
        assert_eq!(
            lease_urls("minio", &console, None, &leases(&[("console", 24000)])).unwrap(),
            urls(&[("console", "http://localhost:24000")])
        );
    }

    #[test]
    fn a_compose_service_with_a_driver_is_reached_with_the_scheme_of_the_driver() {
        let dir = tempfile::tempdir().unwrap();
        let postgres = Driver::load("postgres", dir.path(), None).unwrap();
        let db =
            declared("compose = 'db'\ndriver = 'postgres'\nports = { db = 5432 }\nresources.dev = { name = 'shop' }\n");
        assert_eq!(
            lease_urls("db", &db, Some(&postgres), &leases(&[("db", 24000)])).unwrap(),
            urls(&[("db", "postgresql://localhost:24000")])
        );
    }

    #[test]
    fn a_compose_postgres_as_procly_init_proposes_it_is_reached_with_the_scheme_of_the_bundled_driver() {
        // The bundled credentials get a Driver from `procly init`, other ones none.
        for (credentials, password, bundled) in [("bundled", "postgres", true), ("custom", "secret", false)] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                dir.path().join("compose.yaml"),
                format!(
                    "services:\n  db:\n    image: postgres:17\n    environment:\n      POSTGRES_PASSWORD: {password}\n    \
                     ports:\n      - \"5432:5432\"\n"
                ),
            )
            .unwrap();
            let config: Config = toml::from_str(&crate::proposal::propose(dir.path(), "acme").unwrap().toml).unwrap();
            let db = &config.services["db"];
            assert_eq!(db.driver.is_some(), bundled, "{credentials} credentials");

            let driver = scheme_driver("db", db, dir.path(), None, || Ok(dir.path().to_owned())).unwrap();

            assert_eq!(
                lease_urls("db", db, driver.as_ref(), &leases(&[("db", 24000)])).unwrap(),
                urls(&[("db", "postgresql://localhost:24000")]),
                "{credentials} credentials"
            );
        }
    }

    #[test]
    fn a_compose_service_of_an_image_without_a_bundled_driver_has_no_scheme_driver() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("compose.yaml"),
            "services:\n  db:\n    image: mysql:8\n    ports:\n      - \"3306:3306\"\n",
        )
        .unwrap();
        let db = declared("compose = 'db'\nports = { db = 3306 }\n");

        let driver = scheme_driver("db", &db, dir.path(), None, || Ok(dir.path().to_owned())).unwrap();

        assert!(driver.is_none());
        assert_eq!(
            lease_urls("db", &db, driver.as_ref(), &leases(&[("db", 24000)])).unwrap(),
            urls(&[])
        );
    }
}

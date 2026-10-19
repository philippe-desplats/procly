//! Derived variables: environment variables whose values are templates over Leases and Worktree facts (ADR 0007).
//!
//! A template holds 4 kinds of placeholder, `{services.<service>.ports.<port name>}` for the Lease of a Port name of
//! a Service, `{services.<service>.resources.<key>.name}` and `.url` for a Logical resource of a Compose service,
//! `{worktree.slug}` and `{worktree.is_main}`. Every other brace is left alone, but a brace that opens like
//! ours and names nothing is an error, so that a typo never reaches a process as a literal.

use std::collections::{BTreeMap, BTreeSet};

use crate::config::ServiceConfig;
use crate::placeholder::{Piece, pieces};

/// What opens a placeholder of ours, which makes the rest of its braces ours to judge.
const PLACEHOLDER_ROOTS: [&str; 3] = ["worktree", "services", "ports"];

/// The variables Procly injects itself, which a Derived variable cannot replace.
const INJECTED: [&str; 3] = ["PROCLY", "PROCLY_SERVICE", "PORT"];
/// What opens the name of the variables carrying the Leases, such as `PORT_HTTP`.
const LEASE_PREFIX: &str = "PORT_";

/// The leased port of each Port name of each Service, and the facts about the Worktree, that templates read.
#[derive(Debug)]
pub(crate) struct Facts<'a> {
    pub(crate) slug: &'a str,
    pub(crate) is_main: bool,
    /// Service, then Port name, then leased port.
    pub(crate) leases: &'a BTreeMap<String, BTreeMap<String, u16>>,
    /// Service, then resource key, then the Logical resource of this Worktree.
    pub(crate) resources: &'a BTreeMap<String, BTreeMap<String, Resource>>,
}

/// A Logical resource of a Compose service in one Worktree, as a template reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resource {
    pub(crate) name: String,
    pub(crate) url: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum DerivedError {
    #[error(
        "{name:?} cannot name a Derived variable of service {service}, it must be letters, digits and underscores, not starting with a digit"
    )]
    InvalidName { service: String, name: String },
    #[error("service {service} cannot derive {name}, Procly injects it itself")]
    ReservedName { service: String, name: String },
    #[error(
        "Derived variable {name} of service {service} holds the placeholder {{{placeholder}}}, which is none of {{services.<service>.ports.<port name>}}, {{services.<service>.resources.<key>.name}}, {{services.<service>.resources.<key>.url}}, {{worktree.slug}} and {{worktree.is_main}}"
    )]
    UnknownPlaceholder {
        service: String,
        name: String,
        placeholder: String,
    },
    #[error("Derived variable {name} of service {service} refers to service {referenced}, which is not declared")]
    UnknownService {
        service: String,
        name: String,
        referenced: String,
    },
    #[error(
        "Derived variable {name} of service {service} refers to Port name {port_name}, which service {referenced} does not declare"
    )]
    UnknownPortName {
        service: String,
        name: String,
        referenced: String,
        port_name: String,
    },
    #[error(
        "Derived variable {name} of service {service} refers to resource {resource}, which service {referenced} does not declare"
    )]
    UnknownResource {
        service: String,
        name: String,
        referenced: String,
        resource: String,
    },
    #[error(
        "services {first} and {second} give Derived variable {name} different templates, the Env file holds one value per variable"
    )]
    Conflict {
        name: String,
        first: String,
        second: String,
    },
    #[error("no Lease for Port name {port_name} of service {service}")]
    MissingLease { service: String, port_name: String },
    #[error("no Logical resource {resource} known for service {service}")]
    MissingResource { service: String, resource: String },
}

/// A brace of a template that opens like a placeholder of ours and is none.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("unknown placeholder {{{0}}}")]
pub(crate) struct UnknownPlaceholder(pub(crate) String);

/// One piece of a template: text kept as it is, or a placeholder replaced by a fact.
#[derive(Debug, PartialEq, Eq)]
enum Part {
    Text(String),
    Port { service: String, port_name: String },
    ResourceName { service: String, resource: String },
    ResourceUrl { service: String, resource: String },
    WorktreeSlug,
    WorktreeIsMain,
}

/// A template parsed from the text of a Derived variable.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Template {
    parts: Vec<Part>,
}

impl Template {
    pub(crate) fn parse(text: &str) -> Result<Self, UnknownPlaceholder> {
        let mut parts = Vec::new();
        let mut literal = String::new();
        for piece in pieces(text) {
            match piece {
                Piece::Braced(name) if is_ours(name) => {
                    let part = placeholder(name).ok_or_else(|| UnknownPlaceholder(name.to_owned()))?;
                    if !literal.is_empty() {
                        parts.push(Part::Text(std::mem::take(&mut literal)));
                    }
                    parts.push(part);
                }
                Piece::Braced(name) => {
                    literal.push('{');
                    literal.push_str(name);
                    literal.push('}');
                }
                Piece::Text(text) => literal.push_str(text),
            }
        }
        if !literal.is_empty() {
            parts.push(Part::Text(literal));
        }
        Ok(Self { parts })
    }

    /// Every Port name of a Service the template reads, as the Service and the Port name.
    fn references(&self) -> impl Iterator<Item = (&str, &str)> {
        self.parts.iter().filter_map(|part| match part {
            Part::Port { service, port_name } => Some((service.as_str(), port_name.as_str())),
            _ => None,
        })
    }

    /// Every Logical resource the template reads, as the Service and the resource key.
    fn resource_references(&self) -> impl Iterator<Item = (&str, &str)> {
        self.parts.iter().filter_map(|part| match part {
            Part::ResourceName { service, resource } | Part::ResourceUrl { service, resource } => {
                Some((service.as_str(), resource.as_str()))
            }
            _ => None,
        })
    }

    pub(crate) fn render(&self, facts: &Facts<'_>) -> Result<String, DerivedError> {
        let mut rendered = String::new();
        for part in &self.parts {
            match part {
                Part::Text(text) => rendered.push_str(text),
                Part::WorktreeSlug => rendered.push_str(facts.slug),
                Part::WorktreeIsMain => rendered.push_str(if facts.is_main { "true" } else { "false" }),
                Part::Port { service, port_name } => {
                    let port = facts
                        .leases
                        .get(service)
                        .and_then(|leases| leases.get(port_name))
                        .ok_or_else(|| DerivedError::MissingLease {
                            service: service.clone(),
                            port_name: port_name.clone(),
                        })?;
                    rendered.push_str(&port.to_string());
                }
                Part::ResourceName { service, resource } | Part::ResourceUrl { service, resource } => {
                    let found = facts
                        .resources
                        .get(service)
                        .and_then(|resources| resources.get(resource))
                        .ok_or_else(|| DerivedError::MissingResource {
                            service: service.clone(),
                            resource: resource.clone(),
                        })?;
                    rendered.push_str(if matches!(part, Part::ResourceName { .. }) {
                        &found.name
                    } else {
                        &found.url
                    });
                }
            }
        }
        Ok(rendered)
    }
}

/// Whether the text between 2 braces opens like a placeholder of ours.
fn is_ours(name: &str) -> bool {
    PLACEHOLDER_ROOTS
        .iter()
        .any(|root| name == *root || name.strip_prefix(root).is_some_and(|rest| rest.starts_with('.')))
}

/// The placeholder named `name`, the text between its braces.
fn placeholder(name: &str) -> Option<Part> {
    Some(match name.split('.').collect::<Vec<_>>()[..] {
        ["worktree", "slug"] => Part::WorktreeSlug,
        ["worktree", "is_main"] => Part::WorktreeIsMain,
        ["services", service, "ports", port_name] if is_name(service) && is_name(port_name) => Part::Port {
            service: service.to_owned(),
            port_name: port_name.to_owned(),
        },
        ["services", service, "resources", resource, field @ ("name" | "url")]
            if is_name(service) && is_name(resource) =>
        {
            let (service, resource) = (service.to_owned(), resource.to_owned());
            if field == "name" {
                Part::ResourceName { service, resource }
            } else {
                Part::ResourceUrl { service, resource }
            }
        }
        _ => return None,
    })
}

/// Whether `name` fits in a placeholder: a Service name or a Port name.
fn is_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A variable name every shell and dotenv parser reads the same way.
fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The Derived variables of every Service of a configuration, parsed and checked against it.
#[derive(Debug, Default)]
pub(crate) struct Derived {
    /// Service, then variable name, then its template.
    services: BTreeMap<String, BTreeMap<String, Template>>,
}

impl Derived {
    /// Parses the `env` table of each Service. Fails on a name that is no identifier or that Procly injects, on a
    /// placeholder that names nothing, on a Service or a Port name no Service declares, and on 2 Services giving
    /// one variable 2 templates.
    pub(crate) fn from_config(configured: &BTreeMap<String, ServiceConfig>) -> Result<Self, DerivedError> {
        let mut services: BTreeMap<String, BTreeMap<String, Template>> = BTreeMap::new();
        // The Service that first declared each variable, to name it in a conflict.
        let mut declared_by: BTreeMap<&str, (&str, &str)> = BTreeMap::new();
        for (service, config) in configured {
            let mut variables = BTreeMap::new();
            for (name, text) in &config.env {
                let (service_name, variable) = (service.clone(), name.clone());
                if !is_identifier(name) {
                    return Err(DerivedError::InvalidName {
                        service: service_name,
                        name: variable,
                    });
                }
                if INJECTED.contains(&name.as_str()) || name.starts_with(LEASE_PREFIX) {
                    return Err(DerivedError::ReservedName {
                        service: service_name,
                        name: variable,
                    });
                }
                let template = Template::parse(text).map_err(|UnknownPlaceholder(placeholder)| {
                    DerivedError::UnknownPlaceholder {
                        service: service_name.clone(),
                        name: variable.clone(),
                        placeholder,
                    }
                })?;
                for (referenced, port_name) in template.references() {
                    let Some(target) = configured.get(referenced) else {
                        return Err(DerivedError::UnknownService {
                            service: service_name,
                            name: variable,
                            referenced: referenced.to_owned(),
                        });
                    };
                    if !target.ports.contains_key(port_name) {
                        return Err(DerivedError::UnknownPortName {
                            service: service_name,
                            name: variable,
                            referenced: referenced.to_owned(),
                            port_name: port_name.to_owned(),
                        });
                    }
                }
                for (referenced, resource) in template.resource_references() {
                    let Some(target) = configured.get(referenced) else {
                        return Err(DerivedError::UnknownService {
                            service: service_name,
                            name: variable,
                            referenced: referenced.to_owned(),
                        });
                    };
                    if !target.resources.contains_key(resource) {
                        return Err(DerivedError::UnknownResource {
                            service: service_name,
                            name: variable,
                            referenced: referenced.to_owned(),
                            resource: resource.to_owned(),
                        });
                    }
                }
                match declared_by.get(name.as_str()) {
                    Some((first, first_text)) if first_text != text => {
                        return Err(DerivedError::Conflict {
                            name: name.clone(),
                            first: (*first).to_owned(),
                            second: service.clone(),
                        });
                    }
                    Some(_) => {}
                    None => {
                        declared_by.insert(name, (service, text));
                    }
                }
                variables.insert(name.clone(), template);
            }
            services.insert(service.clone(), variables);
        }
        Ok(Self { services })
    }

    /// Every Service some template reads a Lease of, or a resource of whose URL holds a port, whose Leases must be
    /// granted before rendering.
    pub(crate) fn referenced(&self) -> BTreeSet<&str> {
        self.services
            .values()
            .flat_map(BTreeMap::values)
            .flat_map(|template| {
                template
                    .references()
                    .chain(template.resource_references())
                    .map(|(service, _)| service)
            })
            .collect()
    }

    /// Every Service some template reads a Logical resource of, whose names and URLs must be worked out before
    /// rendering.
    pub(crate) fn resource_services(&self) -> BTreeSet<&str> {
        self.services
            .values()
            .flat_map(BTreeMap::values)
            .flat_map(|template| template.resource_references().map(|(service, _)| service))
            .collect()
    }

    /// The variables of `service`, rendered and sorted by name.
    pub(crate) fn render(&self, service: &str, facts: &Facts<'_>) -> Result<Vec<(String, String)>, DerivedError> {
        render_variables(self.services.get(service).into_iter(), facts)
    }

    /// The variables of every Service, rendered and sorted by name, for the Env file.
    pub(crate) fn render_all(&self, facts: &Facts<'_>) -> Result<Vec<(String, String)>, DerivedError> {
        render_variables(self.services.values(), facts)
    }
}

fn render_variables<'a>(
    services: impl Iterator<Item = &'a BTreeMap<String, Template>>,
    facts: &Facts<'_>,
) -> Result<Vec<(String, String)>, DerivedError> {
    let mut rendered = BTreeMap::new();
    for variables in services {
        for (name, template) in variables {
            rendered.insert(name.clone(), template.render(facts)?);
        }
    }
    Ok(rendered.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leases() -> BTreeMap<String, BTreeMap<String, u16>> {
        BTreeMap::from([
            (
                "api".to_owned(),
                BTreeMap::from([("http".to_owned(), 24000), ("test".to_owned(), 24001)]),
            ),
            ("web".to_owned(), BTreeMap::from([("http".to_owned(), 24002)])),
        ])
    }

    fn render(text: &str, slug: &str, is_main: bool) -> String {
        let leases = leases();
        Template::parse(text)
            .unwrap()
            .render(&Facts {
                slug,
                is_main,
                leases: &leases,
                resources: &BTreeMap::new(),
            })
            .unwrap()
    }

    #[test]
    fn a_template_embeds_the_worktree_slug() {
        assert_eq!(
            render("session_{worktree.slug}", "app_other", false),
            "session_app_other"
        );
    }

    #[test]
    fn a_template_embeds_the_lease_of_another_service() {
        assert_eq!(
            render("http://localhost:{services.api.ports.http}/v1", "app", true),
            "http://localhost:24000/v1"
        );
        assert_eq!(
            render("{services.api.ports.test},{services.web.ports.http}", "app", true),
            "24001,24002"
        );
    }

    #[test]
    fn a_template_tells_whether_the_worktree_is_the_main_one() {
        assert_eq!(render("{worktree.is_main}", "app", true), "true");
        assert_eq!(render("{worktree.is_main}", "app", false), "false");
    }

    #[test]
    fn braces_that_are_no_placeholder_of_ours_are_left_alone() {
        let text = r#"${HOME} {} { "a": 1 } {services.api {worktree.slug"#;
        assert_eq!(render(text, "app", true), text);
        assert_eq!(render("{{worktree.slug}}", "app", true), "{app}");
    }

    #[test]
    fn a_typo_in_a_placeholder_of_ours_is_an_error_not_a_literal() {
        for placeholder in [
            "worktree.slg",
            "worktree",
            "services.api.port.http",
            "services.api.ports",
            "services..ports.http",
            "ports.http",
        ] {
            assert_eq!(
                Template::parse(&format!("a{{{placeholder}}}b")),
                Err(UnknownPlaceholder(placeholder.to_owned())),
                "{placeholder}"
            );
        }
    }

    fn services(text: &str) -> BTreeMap<String, crate::config::ServiceConfig> {
        toml::from_str::<crate::config::Config>(text).unwrap().services
    }

    fn derived(text: &str) -> Result<Derived, DerivedError> {
        Derived::from_config(&services(text))
    }

    const API_AND_WEB: &str = "[services.api]\nrun = 'api'\nports = { http = 3000 }\n\
        [services.web]\nrun = 'web'\nports = { http = 3001 }\n\
        [services.web.env]\nAPI_URL = 'http://localhost:{services.api.ports.http}'\nCOOKIE = 'web_{worktree.slug}'\n";

    #[test]
    fn a_service_renders_its_own_variables_sorted_by_name() {
        let derived = derived(API_AND_WEB).unwrap();
        let leases = leases();
        let facts = Facts {
            slug: "app_other",
            is_main: false,
            leases: &leases,
            resources: &BTreeMap::new(),
        };
        assert_eq!(
            derived.render("web", &facts).unwrap(),
            [
                ("API_URL".to_owned(), "http://localhost:24000".to_owned()),
                ("COOKIE".to_owned(), "web_app_other".to_owned()),
            ]
        );
        assert_eq!(derived.render("api", &facts).unwrap(), []);
    }

    #[test]
    fn the_services_a_template_refers_to_are_listed() {
        let derived = derived(API_AND_WEB).unwrap();
        assert_eq!(derived.referenced().into_iter().collect::<Vec<_>>(), ["api"]);
    }

    #[test]
    fn a_template_must_refer_to_a_declared_service_and_one_of_its_port_names() {
        let unknown_service = derived("[services.web]\nrun = 'x'\nenv = { A = '{services.db.ports.http}' }\n");
        assert_eq!(
            unknown_service.unwrap_err(),
            DerivedError::UnknownService {
                service: "web".to_owned(),
                name: "A".to_owned(),
                referenced: "db".to_owned()
            }
        );
        let unknown_port =
            derived("[services.web]\nrun = 'x'\nports = { http = 1 }\nenv = { A = '{services.web.ports.https}' }\n");
        assert_eq!(
            unknown_port.unwrap_err(),
            DerivedError::UnknownPortName {
                service: "web".to_owned(),
                name: "A".to_owned(),
                referenced: "web".to_owned(),
                port_name: "https".to_owned()
            }
        );
    }

    #[test]
    fn a_name_must_be_a_shell_identifier_that_procly_does_not_inject() {
        for name in [
            "1A",
            "A-B",
            "A B",
            "",
            "A=B",
            "PROCLY",
            "PROCLY_SERVICE",
            "PORT",
            "PORT_HTTP",
        ] {
            let table = format!("[services.web]\nrun = 'x'\n[services.web.env]\n'{name}' = 'v'\n");
            let error = derived(&table).unwrap_err();
            assert!(
                matches!(
                    error,
                    DerivedError::InvalidName { .. } | DerivedError::ReservedName { .. }
                ),
                "{name:?} gave {error:?}"
            );
        }
        assert!(derived("[services.web]\nrun = 'x'\n[services.web.env]\n_a1 = 'v'\nPORTAL = 'v'\n").is_ok());
    }

    #[test]
    fn two_services_may_share_a_variable_only_with_the_same_template() {
        let same = "[services.a]\nrun = 'x'\nenv = { V = '{worktree.slug}' }\n[services.b]\nrun = 'x'\nenv = { V = '{worktree.slug}' }\n";
        let derived_same = derived(same).unwrap();
        let leases = BTreeMap::new();
        let facts = Facts {
            slug: "s",
            is_main: true,
            leases: &leases,
            resources: &BTreeMap::new(),
        };
        assert_eq!(
            derived_same.render_all(&facts).unwrap(),
            [("V".to_owned(), "s".to_owned())]
        );

        let different = "[services.a]\nrun = 'x'\nenv = { V = '1' }\n[services.b]\nrun = 'x'\nenv = { V = '2' }\n";
        assert_eq!(
            derived(different).unwrap_err(),
            DerivedError::Conflict {
                name: "V".to_owned(),
                first: "a".to_owned(),
                second: "b".to_owned()
            }
        );
    }

    const DB_AND_API: &str = "[services.db]\ncompose = 'postgres'\ndriver = 'postgres'\nports = { db = 5432 }\n\
        [services.db.resources.dev]\nname = 'app'\n\
        [services.api]\nrun = 'api'\n\
        [services.api.env]\nDATABASE_URL = '{services.db.resources.dev.url}'\nDATABASE_NAME = '{services.db.resources.dev.name}'\n";

    fn resources() -> BTreeMap<String, BTreeMap<String, Resource>> {
        BTreeMap::from([(
            "db".to_owned(),
            BTreeMap::from([(
                "dev".to_owned(),
                Resource {
                    name: "app_other".to_owned(),
                    url: "postgresql://localhost:24000/app_other".to_owned(),
                },
            )]),
        )])
    }

    #[test]
    fn a_template_embeds_the_name_and_the_connection_url_of_a_logical_resource() {
        let derived = derived(DB_AND_API).unwrap();
        let (leases, resources) = (leases(), resources());
        let facts = Facts {
            slug: "other",
            is_main: false,
            leases: &leases,
            resources: &resources,
        };
        assert_eq!(
            derived.render("api", &facts).unwrap(),
            [
                ("DATABASE_NAME".to_owned(), "app_other".to_owned()),
                (
                    "DATABASE_URL".to_owned(),
                    "postgresql://localhost:24000/app_other".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn the_service_holding_a_resource_a_template_reads_is_listed_with_its_resources() {
        let derived = derived(DB_AND_API).unwrap();
        assert_eq!(derived.resource_services().into_iter().collect::<Vec<_>>(), ["db"]);
        // Its URL holds a port, so its Leases are granted before rendering.
        assert_eq!(derived.referenced().into_iter().collect::<Vec<_>>(), ["db"]);
    }

    #[test]
    fn a_template_must_refer_to_a_declared_resource_of_a_declared_service() {
        let unknown_service = derived("[services.web]\nrun = 'x'\nenv = { A = '{services.db.resources.dev.url}' }\n");
        assert_eq!(
            unknown_service.unwrap_err(),
            DerivedError::UnknownService {
                service: "web".to_owned(),
                name: "A".to_owned(),
                referenced: "db".to_owned()
            }
        );
        let unknown_resource = derived(&DB_AND_API.replace("resources.dev.url", "resources.test.url"));
        assert_eq!(
            unknown_resource.unwrap_err(),
            DerivedError::UnknownResource {
                service: "api".to_owned(),
                name: "DATABASE_URL".to_owned(),
                referenced: "db".to_owned(),
                resource: "test".to_owned()
            }
        );
    }

    #[test]
    fn a_resource_the_facts_lack_is_an_error() {
        let template = Template::parse("{services.db.resources.dev.url}").unwrap();
        let leases = BTreeMap::new();
        assert_eq!(
            template.render(&Facts {
                slug: "s",
                is_main: true,
                leases: &leases,
                resources: &BTreeMap::new(),
            }),
            Err(DerivedError::MissingResource {
                service: "db".to_owned(),
                resource: "dev".to_owned()
            })
        );
    }

    #[test]
    fn a_placeholder_for_a_resource_names_its_url_or_its_name_and_nothing_else() {
        assert!(Template::parse("{services.db.resources.dev.url}").is_ok());
        assert!(Template::parse("{services.db.resources.dev.name}").is_ok());
        for placeholder in [
            "services.db.resources.dev",
            "services.db.resources.dev.host",
            "services.db.resources..url",
        ] {
            assert_eq!(
                Template::parse(&format!("{{{placeholder}}}")),
                Err(UnknownPlaceholder(placeholder.to_owned())),
                "{placeholder}"
            );
        }
    }

    #[test]
    fn a_lease_the_facts_lack_is_an_error() {
        let leases = BTreeMap::new();
        let template = Template::parse("{services.api.ports.http}").unwrap();
        assert_eq!(
            template.render(&Facts {
                slug: "s",
                is_main: true,
                leases: &leases,
                resources: &BTreeMap::new(),
            }),
            Err(DerivedError::MissingLease {
                service: "api".to_owned(),
                port_name: "http".to_owned()
            })
        );
    }
}

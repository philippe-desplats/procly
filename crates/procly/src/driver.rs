//! Drivers: declarative TOML files describing how to create, check and drop the Logical resources of one technology
//! inside its container, and how to build their connection URL (ADR 0005).
//!
//! The bundled Drivers use the same format as the ones a user or a Repository adds, so a new technology never needs a
//! new Procly release. A Driver is looked up in the Repository, then in the user's directory, then among the bundled
//! ones, so the nearest file wins.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::placeholder::{self, Piece};

/// Where the Drivers of a Repository live, relative to the root of a Worktree.
pub(crate) const REPOSITORY_DIR: &str = ".procly/drivers";
/// Where the Drivers of a user live, relative to the directory of the user settings.
pub(crate) const USER_DIR: &str = "drivers";

/// The Drivers that ship with Procly, in the format any other Driver uses.
const BUNDLED: [(&str, &str); 2] = [
    ("postgres", include_str!("../drivers/postgres.toml")),
    ("redis", include_str!("../drivers/redis.toml")),
];

/// The directory of the Drivers of the user, next to the user settings. `None` when none of the variables that locate
/// them is set.
pub(crate) fn user_dir() -> Option<PathBuf> {
    Some(crate::config::user_settings()?.parent()?.join(USER_DIR))
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum DriverError {
    #[error("cannot read {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid Driver {origin}")]
    Parse {
        origin: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("no Driver named {name}, add {name}.toml to one of {}", .searched.iter().map(|dir| dir.display().to_string()).collect::<Vec<_>>().join(", "))]
    Unknown { name: String, searched: Vec<PathBuf> },
    #[error("Driver {driver} gives the resource {name:?} {length} characters, its limit is {max}, shorten the name")]
    NameTooLong {
        driver: String,
        name: String,
        length: usize,
        max: usize,
    },
    #[error("invalid Driver {origin}: {reason}")]
    Index { origin: String, reason: &'static str },
    #[error(
        "resource {resource} of service {service} sets no `name`, which the {driver} Driver needs to name it in the container"
    )]
    NameRequired {
        driver: String,
        service: String,
        resource: String,
    },
    #[error(
        "resource {resource} of service {service} sets `name`, but the {driver} Driver numbers its resources itself, remove it"
    )]
    NameNotUsed {
        driver: String,
        service: String,
        resource: String,
    },
    #[error("the URL of Driver {driver} uses {{port}}, but the service declares no Port name")]
    NoPort { driver: String },
    #[error("the URL of Driver {driver} uses {{ports.{name}}}, but {name} is not one of the Port names of the service")]
    UnknownPort { driver: String, name: String },
}

/// The number of indexes of a Driver file that numbers its resources, which needs a capacity and has no names to
/// shorten.
fn index_capacity(file: &DriverFile, origin: &str) -> Result<Option<u32>, DriverError> {
    if !file.index {
        return Ok(None);
    }
    let refused = |reason| DriverError::Index {
        origin: origin.to_owned(),
        reason,
    };
    match file.capacity {
        Some(capacity) if capacity > 0 => {
            if file.max_length.is_some() {
                return Err(refused("`max_length` is for names, and `index` numbers the resources"));
            }
            Ok(Some(capacity))
        }
        _ => Err(refused("`index` needs a `capacity` of at least 1")),
    }
}

/// What a Driver file holds.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DriverFile {
    /// The connection URL, with `{resource}` for the name of the resource, `{port}` for the Lease the health check of
    /// the service probes and `{ports.<name>}` for the Lease of a Port name.
    url: String,
    /// The longest name the technology accepts for a resource, refused rather than silently truncated.
    max_length: Option<usize>,
    /// How many resources the container holds at most, enforced for the Drivers that number their resources.
    capacity: Option<u32>,
    /// Whether the resources are the numbers 0 to `capacity` minus 1, handed out by Procly one per resource and
    /// Worktree, such as the databases of a Redis server, which exist from the start.
    #[serde(default)]
    index: bool,
    /// A command run in the container, succeeding once it accepts the commands below.
    ready: Option<String>,
    /// A command run in the container, succeeding when the resource exists.
    check: String,
    /// A command run in the container, creating an empty resource.
    create: String,
    /// A command run in the container, creating the resource as a copy of `{template}`.
    copy: Option<String>,
    /// A command run in the container, removing the resource, used only to undo a resource whose Seed failed.
    drop: String,
}

/// A Driver, ready to be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Driver {
    pub(crate) name: String,
    url: String,
    max_length: Option<usize>,
    /// The number of indexes of a Driver that numbers its resources.
    index_capacity: Option<u32>,
    pub(crate) ready: Option<String>,
    pub(crate) check: String,
    pub(crate) create: String,
    pub(crate) copy: Option<String>,
    pub(crate) drop: String,
}

impl Driver {
    /// Reads the Driver `name` from the nearest of the Repository, the user's directory and the bundled ones.
    ///
    /// `worktree` is the root of the Worktree asking and `user_dir` the directory holding the user's Drivers.
    pub(crate) fn load(name: &str, worktree: &Path, user_dir: Option<&Path>) -> Result<Self, DriverError> {
        let mut searched = vec![worktree.join(REPOSITORY_DIR)];
        searched.extend(user_dir.map(Path::to_path_buf));
        for dir in &searched {
            let path = dir.join(format!("{name}.toml"));
            match std::fs::read_to_string(&path) {
                Ok(text) => return Self::parse(name, &text, &path.display().to_string()),
                Err(source) if source.kind() == io::ErrorKind::NotFound => {}
                Err(source) => return Err(DriverError::Read { path, source }),
            }
        }
        match BUNDLED.iter().find(|(bundled, _)| *bundled == name) {
            Some((_, text)) => Self::parse(name, text, &format!("{name} (bundled)")),
            None => Err(DriverError::Unknown {
                name: name.to_owned(),
                searched,
            }),
        }
    }

    fn parse(name: &str, text: &str, origin: &str) -> Result<Self, DriverError> {
        let file: DriverFile = toml::from_str(text).map_err(|source| DriverError::Parse {
            origin: origin.to_owned(),
            source,
        })?;
        let index_capacity = index_capacity(&file, origin)?;
        Ok(Self {
            name: name.to_owned(),
            url: file.url,
            max_length: file.max_length,
            index_capacity,
            ready: file.ready,
            check: file.check,
            create: file.create,
            copy: file.copy,
            drop: file.drop,
        })
    }

    /// How many indexes the container holds when the Driver numbers its resources, `None` when it names them.
    pub(crate) fn index_capacity(&self) -> Option<u32> {
        self.index_capacity
    }

    /// The name of a resource in a Worktree: the declared name in the Main worktree, which keeps the names of the
    /// configuration, and the declared name, a double underscore and the slug anywhere else.
    ///
    /// A declared name holds no double underscore and a slug neither, so the first one is where to split, and 2
    /// different resources never get one name, in the same Worktree or in 2 of them.
    pub(crate) fn resource_name(&self, declared: &str, slug: &str, main: bool) -> Result<String, DriverError> {
        let name = if main {
            declared.to_owned()
        } else {
            format!("{declared}__{slug}")
        };
        match self.max_length {
            Some(max) if name.len() > max => Err(DriverError::NameTooLong {
                driver: self.name.clone(),
                length: name.len(),
                name,
                max,
            }),
            _ => Ok(name),
        }
    }

    /// The connection URL of the resource `name`, given the Leases of the service and the one its health check probes.
    pub(crate) fn url(
        &self,
        name: &str,
        leases: &BTreeMap<String, u16>,
        health_port: Option<u16>,
    ) -> Result<String, DriverError> {
        placeholder::render(&self.url, |braced| match braced {
            "resource" => Ok(Some(name.to_owned())),
            "port" => health_port
                .map(|port| Some(port.to_string()))
                .ok_or_else(|| DriverError::NoPort {
                    driver: self.name.clone(),
                }),
            // Any other brace is not ours, kept as it is.
            _ => braced
                .strip_prefix("ports.")
                .map(|port_name| {
                    leases
                        .get(port_name)
                        .map(u16::to_string)
                        .ok_or_else(|| DriverError::UnknownPort {
                            driver: self.name.clone(),
                            name: port_name.to_owned(),
                        })
                })
                .transpose(),
        })
    }

    /// The scheme of the connection URL when the URL reaches the Lease `port_name`, the one the health check probes
    /// when `health` is set, such as `postgresql` for the bundled Postgres Driver.
    pub(crate) fn scheme_of(&self, port_name: &str, health: bool) -> Option<&str> {
        let reaches = placeholder::pieces(&self.url).iter().any(|piece| match piece {
            Piece::Braced("port") => health,
            Piece::Braced(braced) => braced.strip_prefix("ports.") == Some(port_name),
            Piece::Text(_) => false,
        });
        let (scheme, _) = self.url.split_once("://")?;
        (reaches && !scheme.is_empty()).then_some(scheme)
    }

    /// A command of the Driver with `{resource}` and `{template}` replaced, both already checked to be plain words.
    pub(crate) fn command(command: &str, resource: &str, template: Option<&str>) -> String {
        let Ok(rendered) = placeholder::render(command, |braced| {
            Ok::<_, Infallible>(match braced {
                "resource" => Some(resource.to_owned()),
                "template" => template.map(str::to_owned),
                _ => None,
            })
        });
        rendered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leases() -> BTreeMap<String, u16> {
        BTreeMap::from([("db".to_owned(), 24000), ("admin".to_owned(), 24001)])
    }

    fn driver(text: &str) -> Driver {
        Driver::parse("test", text, "test").unwrap()
    }

    const MINIMAL: &str = "url = 'db://localhost:{port}/{resource}'\ncheck = 'c'\ncreate = 'n'\ndrop = 'd'\n";

    #[test]
    fn the_bundled_postgres_driver_is_a_driver_file_like_any_other() {
        let dir = tempfile::tempdir().unwrap();
        let postgres = Driver::load("postgres", dir.path(), None).unwrap();
        assert_eq!(postgres.name, "postgres");
        assert!(postgres.copy.is_some());
        assert!(postgres.ready.is_some());
        assert_eq!(
            postgres.url("app", &leases(), Some(24000)).unwrap(),
            "postgresql://postgres:postgres@localhost:24000/app"
        );
    }

    #[test]
    fn the_s3_driver_of_the_configuration_reference_loads_and_names_buckets_within_their_limit() {
        let reference =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/configuration.md")).unwrap();
        let (_, section) = reference
            .split_once("### Example: a Driver for an S3-compatible store")
            .expect("the reference documents the S3 Driver");
        let (_, block) = section
            .split_once("```toml\n")
            .expect("the section shows a Driver file");
        let (text, _) = block.split_once("```").expect("the Driver file is fenced");
        let s3 = Driver::parse("s3", text, "docs/configuration.md").unwrap();
        assert_eq!(
            s3.url("assets", &leases(), Some(24000)).unwrap(),
            "http://localhost:24000"
        );
        assert!(s3.copy.is_none());
        assert_eq!(s3.index_capacity(), None);
        let longest = "a".repeat(63);
        assert_eq!(s3.resource_name(&longest, "", true).unwrap(), longest);
        assert!(matches!(
            s3.resource_name(&longest, "other", false),
            Err(DriverError::NameTooLong { max: 63, .. })
        ));
    }

    #[test]
    fn the_scheme_of_the_url_is_the_one_of_the_lease_the_url_reaches() {
        let health = driver(MINIMAL);
        assert_eq!(health.scheme_of("db", true), Some("db"));
        assert_eq!(health.scheme_of("admin", false), None);
        let named = driver("url = 'db://localhost:{ports.admin}/{resource}'\ncheck = 'c'\ncreate = 'n'\ndrop = 'd'\n");
        assert_eq!(named.scheme_of("admin", false), Some("db"));
        assert_eq!(named.scheme_of("db", true), None);
        let dir = tempfile::tempdir().unwrap();
        let postgres = Driver::load("postgres", dir.path(), None).unwrap();
        assert_eq!(postgres.scheme_of("db", true), Some("postgresql"));
    }

    #[test]
    fn the_bundled_redis_driver_numbers_its_resources_and_holds_16_of_them() {
        let dir = tempfile::tempdir().unwrap();
        let redis = Driver::load("redis", dir.path(), None).unwrap();
        assert_eq!(redis.name, "redis");
        assert_eq!(redis.index_capacity(), Some(16));
        assert!(redis.ready.is_some());
        assert_eq!(
            redis.url("3", &leases(), Some(24000)).unwrap(),
            "redis://localhost:24000/3"
        );
        assert_eq!(
            Driver::load("postgres", dir.path(), None).unwrap().index_capacity(),
            None
        );
    }

    #[test]
    fn a_driver_numbers_its_resources_only_with_a_capacity_and_without_a_maximum_length() {
        let numbered = |extra: &str| Driver::parse("t", &format!("{MINIMAL}{extra}"), "t");
        assert_eq!(
            numbered("index = true\ncapacity = 4\n").unwrap().index_capacity(),
            Some(4)
        );
        // `capacity` alone only describes a container, the resources keep their names.
        assert_eq!(numbered("capacity = 4\n").unwrap().index_capacity(), None);
        for bad in [
            "index = true\n",
            "index = true\ncapacity = 0\n",
            "index = true\ncapacity = 4\nmax_length = 9\n",
        ] {
            assert!(
                matches!(numbered(bad), Err(DriverError::Index { .. })),
                "{bad:?} was accepted"
            );
        }
    }

    #[test]
    fn a_driver_is_read_from_the_repository_then_the_user_then_the_bundled_ones() {
        let worktree = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        let in_repository = worktree.path().join(REPOSITORY_DIR);
        std::fs::create_dir_all(&in_repository).unwrap();

        std::fs::write(user.path().join("postgres.toml"), MINIMAL.replace("db://", "user://")).unwrap();
        let from_user = Driver::load("postgres", worktree.path(), Some(user.path())).unwrap();
        assert_eq!(from_user.url("x", &leases(), Some(1)).unwrap(), "user://localhost:1/x");

        std::fs::write(in_repository.join("postgres.toml"), MINIMAL.replace("db://", "repo://")).unwrap();
        let from_repository = Driver::load("postgres", worktree.path(), Some(user.path())).unwrap();
        assert_eq!(
            from_repository.url("x", &leases(), Some(1)).unwrap(),
            "repo://localhost:1/x"
        );
    }

    #[test]
    fn a_driver_nobody_wrote_is_an_error_naming_where_it_was_looked_for() {
        let worktree = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        let error = Driver::load("mongo", worktree.path(), Some(user.path())).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("no Driver named mongo"), "{message}");
        assert!(message.contains(&worktree.path().join(REPOSITORY_DIR).display().to_string()));
        assert!(message.contains(&user.path().display().to_string()));
    }

    #[test]
    fn a_driver_file_with_an_unknown_key_or_without_a_command_is_refused() {
        for text in [
            &format!("{MINIMAL}extra = 1\n"),
            "url = 'x'\ncheck = 'c'\ncreate = 'n'\n",
            "check = 'c'\ncreate = 'n'\ndrop = 'd'\n",
        ] {
            assert!(Driver::parse("t", text, "t").is_err(), "{text:?} was accepted");
        }
    }

    #[test]
    fn the_main_worktree_keeps_the_declared_name_and_the_others_append_their_slug() {
        let driver = driver(MINIMAL);
        assert_eq!(driver.resource_name("app", "app_feature", true).unwrap(), "app");
        assert_eq!(
            driver.resource_name("app_test", "feature_x", false).unwrap(),
            "app_test__feature_x"
        );
    }

    #[test]
    fn two_worktrees_never_get_the_same_name_for_different_resources() {
        // `app` in the worktree `test_x` and `app_test` in the worktree `x` would both be `app_test_x` with a single
        // underscore. A name holds no double underscore, a slug neither, so the first one is the split.
        let driver = driver(MINIMAL);
        assert_ne!(
            driver.resource_name("app", "test_x", false).unwrap(),
            driver.resource_name("app_test", "x", false).unwrap()
        );
        // And none of them is the name of a resource of the Main worktree.
        assert_ne!(
            driver.resource_name("app", "test", false).unwrap(),
            driver.resource_name("app_test", "x", true).unwrap()
        );
    }

    #[test]
    fn a_name_longer_than_the_technology_accepts_is_refused_instead_of_truncated() {
        let driver = driver(&format!("{MINIMAL}max_length = 12\n"));
        assert!(driver.resource_name("app", "feature", false).is_ok());
        assert!(matches!(
            driver.resource_name("application", "feature", false),
            Err(DriverError::NameTooLong {
                length: 20,
                max: 12,
                ..
            })
        ));
    }

    #[test]
    fn the_url_reads_the_lease_of_the_health_port_or_of_a_port_name() {
        let driver =
            driver("url = 'x://h:{port}/{resource}?admin={ports.admin}'\ncheck = 'c'\ncreate = 'n'\ndrop = 'd'\n");
        assert_eq!(
            driver.url("app", &leases(), Some(24000)).unwrap(),
            "x://h:24000/app?admin=24001"
        );
        assert!(matches!(
            driver.url("app", &leases(), None),
            Err(DriverError::NoPort { .. })
        ));
        let unknown = Driver::parse(
            "t",
            "url = '{ports.cache}'\ncheck = 'c'\ncreate = 'n'\ndrop = 'd'\n",
            "t",
        )
        .unwrap();
        assert!(matches!(
            unknown.url("app", &leases(), Some(1)),
            Err(DriverError::UnknownPort { name, .. }) if name == "cache"
        ));
    }

    #[test]
    fn a_command_gets_the_resource_and_the_template_and_keeps_every_other_brace() {
        assert_eq!(
            Driver::command(
                "cp -r /data/{template} /data/{resource} && echo ${HOME} {}",
                "app_x",
                Some("base")
            ),
            "cp -r /data/base /data/app_x && echo ${HOME} {}"
        );
    }
}

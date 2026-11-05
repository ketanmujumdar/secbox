use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::names::{BoundHost, SecretName, ServiceName};
use crate::placeholder::{Placeholder, SandboxPlaceholders};
use crate::store::{SecretStore, StoreError};
use crate::value::SecretValue;

/// How a secret reaches the sandbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Delivery {
    /// The VM holds a placeholder, and the host proxy adds the real value for
    /// one exact host.
    Proxy { bound_host: BoundHost },
    /// The real value is put into the environment of the named services at
    /// start. This is the shape nearly every program already reads, so a
    /// project works unchanged, but everything in that container can read it:
    /// its libraries, its child processes, a crash report, `docker inspect`.
    Injected,
    /// The real value is written to a file the named services can read, at
    /// `/run/secrets/<name>`. Narrower than the environment, because
    /// something has to open it deliberately, but the program has to be
    /// willing to read a file. Many official images already are, through
    /// their `_FILE` settings.
    InjectedFile,
    /// The real value is passed as a build secret, opt-in per build.
    BuildSecret,
}

/// Which services may receive one secret, and how. Holds no value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawAssignment", into = "RawAssignment")]
pub struct Assignment {
    name: SecretName,
    delivery: Delivery,
    services: Vec<ServiceName>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAssignment {
    name: SecretName,
    delivery: Delivery,
    services: Vec<ServiceName>,
}

impl TryFrom<RawAssignment> for Assignment {
    type Error = AssignmentError;
    fn try_from(raw: RawAssignment) -> Result<Self, AssignmentError> {
        Self::new(raw.name, raw.delivery, raw.services)
    }
}

impl From<Assignment> for RawAssignment {
    fn from(a: Assignment) -> Self {
        Self {
            name: a.name,
            delivery: a.delivery,
            services: a.services,
        }
    }
}

impl Assignment {
    pub fn new(
        name: SecretName,
        delivery: Delivery,
        services: Vec<ServiceName>,
    ) -> Result<Self, AssignmentError> {
        if services.is_empty() {
            return Err(AssignmentError::NoServices { secret: name });
        }
        let mut seen = BTreeSet::new();
        for service in &services {
            if !seen.insert(service) {
                return Err(AssignmentError::DuplicateService {
                    secret: name,
                    service: service.clone(),
                });
            }
        }
        Ok(Self {
            name,
            delivery,
            services,
        })
    }

    pub fn name(&self) -> &SecretName {
        &self.name
    }

    pub fn delivery(&self) -> &Delivery {
        &self.delivery
    }

    pub fn services(&self) -> &[ServiceName] {
        &self.services
    }

    fn applies_to(&self, service: &ServiceName) -> bool {
        self.services.contains(service)
    }
}

/// All assignments for a project. Each secret appears at most once, so it has
/// exactly one delivery route.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawAssignmentSet", into = "RawAssignmentSet")]
pub struct AssignmentSet {
    assignments: Vec<Assignment>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAssignmentSet {
    assignments: Vec<Assignment>,
}

impl TryFrom<RawAssignmentSet> for AssignmentSet {
    type Error = AssignmentError;
    fn try_from(raw: RawAssignmentSet) -> Result<Self, AssignmentError> {
        Self::new(raw.assignments)
    }
}

impl From<AssignmentSet> for RawAssignmentSet {
    fn from(set: AssignmentSet) -> Self {
        Self {
            assignments: set.assignments,
        }
    }
}

/// A validation failure in assignment metadata. Names only.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AssignmentError {
    #[error("secret {secret} is assigned to no services")]
    NoServices { secret: SecretName },
    #[error("secret {secret} lists service {service} more than once")]
    DuplicateService {
        secret: SecretName,
        service: ServiceName,
    },
    #[error("secret {secret} is assigned more than once")]
    DuplicateSecret { secret: SecretName },
}

/// Something a service receives under its own environment variable name: a
/// real value for an injected or build secret.
#[derive(Debug)]
pub struct DeliveredSecret {
    pub name: SecretName,
    pub value: SecretValue,
}

/// A proxy secret as a service sees it: only the placeholder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyEnv {
    pub name: SecretName,
    pub placeholder: Placeholder,
    pub bound_host: BoundHost,
}

/// What one service may get at start.
///
/// Build secrets are listed by name only, because they are never part of a
/// running service's environment. See [`AssignmentSet::resolve_build_secrets`].
#[derive(Debug)]
pub struct ResolvedService {
    pub service: ServiceName,
    pub proxy: Vec<ProxyEnv>,
    /// Delivered through the environment.
    pub injected: Vec<DeliveredSecret>,
    /// Delivered as a file the service reads.
    pub injected_files: Vec<DeliveredSecret>,
    pub build_secret_names: Vec<SecretName>,
}

/// One host-side proxy binding, used to build the egress credential bindings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyBinding {
    pub name: SecretName,
    pub bound_host: BoundHost,
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("secret {name} is assigned but not in the store")]
    MissingSecret { name: SecretName },
    #[error("proxy secret {name} has no placeholder for this sandbox")]
    MissingPlaceholder { name: SecretName },
    #[error("secret {name} is not a build secret assigned to service {service}")]
    NotABuildSecretForService {
        name: SecretName,
        service: ServiceName,
    },
    #[error("secret store failed for {name}: {source}")]
    Store {
        name: SecretName,
        source: StoreError,
    },
}

impl AssignmentSet {
    pub fn new(assignments: Vec<Assignment>) -> Result<Self, AssignmentError> {
        let mut seen = BTreeSet::new();
        for assignment in &assignments {
            if !seen.insert(assignment.name()) {
                return Err(AssignmentError::DuplicateSecret {
                    secret: assignment.name().clone(),
                });
            }
        }
        Ok(Self { assignments })
    }

    pub fn iter(&self) -> impl Iterator<Item = &Assignment> {
        self.assignments.iter()
    }

    /// Returns only what `service` may get at start: placeholders for proxy
    /// secrets and real values for injected secrets assigned to it.
    ///
    /// The store is read only for injected secrets assigned to this service, so
    /// real proxy and build values are never loaded here. A service with no
    /// assignments, or an unknown one, gets nothing.
    pub fn resolve_for_service(
        &self,
        service: &ServiceName,
        store: &dyn SecretStore,
        placeholders: &SandboxPlaceholders,
    ) -> Result<ResolvedService, ResolveError> {
        let mut resolved = ResolvedService {
            service: service.clone(),
            proxy: Vec::new(),
            injected: Vec::new(),
            injected_files: Vec::new(),
            build_secret_names: Vec::new(),
        };
        for assignment in self.assignments.iter().filter(|a| a.applies_to(service)) {
            let name = assignment.name();
            match assignment.delivery() {
                Delivery::Proxy { bound_host } => {
                    let placeholder = placeholders
                        .get(name)
                        .cloned()
                        .ok_or_else(|| ResolveError::MissingPlaceholder { name: name.clone() })?;
                    resolved.proxy.push(ProxyEnv {
                        name: name.clone(),
                        placeholder,
                        bound_host: bound_host.clone(),
                    });
                }
                Delivery::Injected => resolved.injected.push(DeliveredSecret {
                    name: name.clone(),
                    value: load(store, name)?,
                }),
                Delivery::InjectedFile => resolved.injected_files.push(DeliveredSecret {
                    name: name.clone(),
                    value: load(store, name)?,
                }),
                Delivery::BuildSecret => resolved.build_secret_names.push(name.clone()),
            }
        }
        Ok(resolved)
    }

    /// Returns real values for the build secrets opted in for one build of
    /// `service`. Asking for anything that is not a build secret assigned to
    /// that service is an error, and nothing is returned.
    pub fn resolve_build_secrets(
        &self,
        service: &ServiceName,
        opted_in: &[SecretName],
        store: &dyn SecretStore,
    ) -> Result<Vec<DeliveredSecret>, ResolveError> {
        for name in opted_in {
            let allowed = self.assignments.iter().any(|a| {
                a.name() == name
                    && a.applies_to(service)
                    && matches!(a.delivery(), Delivery::BuildSecret)
            });
            if !allowed {
                return Err(ResolveError::NotABuildSecretForService {
                    name: name.clone(),
                    service: service.clone(),
                });
            }
        }
        opted_in
            .iter()
            .map(|name| {
                Ok(DeliveredSecret {
                    name: name.clone(),
                    value: load(store, name)?,
                })
            })
            .collect()
    }

    /// Lists the proxy secrets and their bound hosts, for the host-side proxy.
    pub fn proxy_bindings(&self) -> Vec<ProxyBinding> {
        self.assignments
            .iter()
            .filter_map(|a| match a.delivery() {
                Delivery::Proxy { bound_host } => Some(ProxyBinding {
                    name: a.name().clone(),
                    bound_host: bound_host.clone(),
                }),
                _ => None,
            })
            .collect()
    }
}

fn load(store: &dyn SecretStore, name: &SecretName) -> Result<SecretValue, ResolveError> {
    store
        .get(name)
        .map_err(|source| ResolveError::Store {
            name: name.clone(),
            source,
        })?
        .ok_or_else(|| ResolveError::MissingSecret { name: name.clone() })
}

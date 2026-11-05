use serde::Serialize;

use crate::names::{SecretName, ServiceName};

/// What happened to a secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    Imported,
    Stored,
    Deleted,
    AssignmentChanged,
    PlaceholderIssued,
    InjectedAtStart,
    DeliveredToBuild,
    UsedByProxy,
    DeliveryDenied,
}

/// An audit record. It holds only validated names and an action, so it has no
/// field that could carry a value, and a [`crate::SecretValue`] is not accepted:
///
/// ```
/// use secrets::{AuditAction, AuditEvent, SecretName};
/// AuditEvent::new(0, AuditAction::Stored, SecretName::new("A").unwrap(), None);
/// ```
///
/// ```compile_fail
/// use secrets::{AuditAction, AuditEvent, SecretValue};
/// AuditEvent::new(0, AuditAction::Stored, SecretValue::new("x"), None);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditEvent {
    unix_seconds: u64,
    action: AuditAction,
    secret: SecretName,
    #[serde(skip_serializing_if = "Option::is_none")]
    service: Option<ServiceName>,
}

impl AuditEvent {
    pub fn new(
        unix_seconds: u64,
        action: AuditAction,
        secret: SecretName,
        service: Option<ServiceName>,
    ) -> Self {
        Self {
            unix_seconds,
            action,
            secret,
            service,
        }
    }

    pub fn unix_seconds(&self) -> u64 {
        self.unix_seconds
    }

    pub fn action(&self) -> AuditAction {
        self.action
    }

    pub fn secret(&self) -> &SecretName {
        &self.secret
    }

    pub fn service(&self) -> Option<&ServiceName> {
        self.service.as_ref()
    }
}

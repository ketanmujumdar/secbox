//! Approval: proof that an approver accepted one exact commit and tree.

use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::review::Review;
use crate::validate::validate_commit_id;

/// What an approver is asked to decide on.
#[derive(Debug)]
pub struct ApprovalRequest<'a> {
    pub commit_sha: &'a str,
    pub tree_sha: &'a str,
    pub review: &'a Review,
}

/// An approver's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Approved by the named identity.
    Approved {
        approver: String,
    },
    Denied {
        reason: String,
    },
}

/// The approver itself failed, as opposed to saying no.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ApproverError(pub String);

/// Decides whether a commit may be pushed. The real one asks a person with Touch
/// ID.
///
/// Implementing this trait is not itself proof of a person: any code in the
/// process can implement it. The [`Approval`] type guarantees that an approver
/// was consulted about this exact commit, and the approver has to guarantee that
/// a person was. Display only the escaped fields of the review.
pub trait Approver {
    fn decide(&self, request: &ApprovalRequest<'_>)
        -> std::result::Result<Decision, ApproverError>;
}

/// Permission to push one commit. It can only come from [`approve`]. It is not
/// `Clone`, `Serialize` or `Deserialize`, and a push consumes it, so one approval
/// is good for one push attempt.
#[derive(Debug)]
pub struct Approval {
    pub(crate) commit: String,
    pub(crate) tree: String,
    pub(crate) approver: String,
    pub(crate) quarantine: PathBuf,
}

impl Approval {
    pub fn commit_sha(&self) -> &str {
        &self.commit
    }
    pub fn tree_sha(&self) -> &str {
        &self.tree
    }
    pub fn approver(&self) -> &str {
        &self.approver
    }
}

/// Ask `approver` about `commit_sha`, which must be the tip of `review`. The
/// approver is not consulted at all if the two differ.
pub fn approve(approver: &dyn Approver, commit_sha: &str, review: &Review) -> Result<Approval> {
    validate_commit_id(commit_sha)?;
    if commit_sha != review.tip() {
        return Err(Error::ApprovalMismatch {
            approved: commit_sha.to_string(),
            reviewed: review.tip().to_string(),
        });
    }
    let request = ApprovalRequest {
        commit_sha,
        tree_sha: review.tree(),
        review,
    };
    match approver.decide(&request).map_err(|err| {
        Error::ApproverFailed(crate::display::escape_for_display(err.0.as_bytes()))
    })? {
        Decision::Approved { approver } => Ok(Approval {
            commit: commit_sha.to_string(),
            tree: review.tree().to_string(),
            approver,
            quarantine: review.quarantine().to_path_buf(),
        }),
        Decision::Denied { reason } => Err(Error::ApprovalDenied(
            crate::display::escape_for_display(reason.as_bytes()),
        )),
    }
}

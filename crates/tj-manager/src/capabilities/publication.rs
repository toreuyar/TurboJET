//! Declared publication capability module.
//!
//! The coordinator implementation lives in `crate::publication` because it also
//! owns the durable push/PR state machine. This module is the capability-facing
//! entry point; it re-exports the parameterized evidence loader so the manager
//! never branches on a flow name.

pub use crate::publication::{
    EngineeringEvidence, PublicationEvidence, RoleEvidence, load_publication_evidence,
    publication_operation_key, publish_draft_pr,
};

//! TJ-039/TJ-040 delivery contracts: external CI identity, signed callbacks,
//! deterministic recommendation state and Forgejo-Actions-compatible adapter.
//!
//! This crate does not deploy, merge or release anything itself. It models the
//! local contract used by the API, persistence and UI; real execution remains
//! with the configured external CI/CD system.

pub mod adapter;
pub mod auth;
pub mod model;

pub use adapter::{
    DeliveryAdapter, DispatchFailure, DispatchReceipt, DispatchRequest, ExternalRun,
    ForgejoActionsAdapter, ForgejoActionsConfig,
};
pub use auth::{
    CALLBACK_EVENT_HEADER, CALLBACK_SIGNATURE_HEADER, CALLBACK_SOURCE_HEADER,
    CALLBACK_TIMESTAMP_HEADER, CallbackAuthError, canonical_payload, sign_callback,
    verify_callback,
};
pub use model::{
    CallbackEvent, CallbackKind, CiState, DecisionAction, DeploymentState, DispatchState,
    ObservationState, Recommendation, ReleaseEnvironment, ReleaseIdentity, ReleaseKind,
    ReleaseState, TransitionError, VerificationState, apply_event, authorization_operation_key,
    recommendation, release_operation_key,
};

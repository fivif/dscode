//! Safety layer — command-blocking, path-containment, and permission prompts.

pub mod guard;
pub mod permission;

pub use guard::{CommandRisk, SafetyGuard};
pub use permission::PermissionHub;

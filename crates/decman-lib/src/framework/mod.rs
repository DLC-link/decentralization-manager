//! Integrator-facing, template-agnostic pieces.

pub mod encode;
pub mod filters;
pub mod record;

mod template_id;
pub use template_id::TemplateId;

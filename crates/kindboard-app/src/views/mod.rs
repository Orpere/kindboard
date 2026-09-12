//! View modules: one per screen/panel of the app.
//!
//! Views are pure UI + state: they render from app state, return commands
//! for the app to enqueue, and never talk to the core directly.

pub mod about;
pub mod cluster;
pub mod deps;
pub mod diagram;
pub mod ops;
pub mod overview;
pub mod wizard;

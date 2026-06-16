pub mod admin;
pub mod auth;
pub mod handlers;
pub mod middleware;
pub mod router;
pub mod ui;

pub use router::build_router;

pub use crate::AppState;

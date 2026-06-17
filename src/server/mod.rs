pub mod admin;
pub mod auth;
pub mod auth_endpoints;
pub mod events;
pub mod handlers;
pub mod middleware;
pub mod multimodal;
pub mod router;
pub mod ui;
pub mod ui_login;

pub use router::build_router;

pub use crate::AppState;

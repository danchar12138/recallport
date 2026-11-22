//! (placeholder until the MCP layer is written)
use crate::{app::App, service::Service};
use axum::Router;
use std::sync::Arc;

pub fn router(_service: Arc<Service>, _app: Arc<App>, _host: &str, _alternate: &str, _base: &str) -> Router<crate::web::Web> {
    Router::new()
}

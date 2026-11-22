//! Data (backups, restore, automatic backup, start with Windows) and connecting AI clients.
use super::Web;
use axum::Router;

pub(super) fn routes() -> Router<Web> {
    Router::new()
}

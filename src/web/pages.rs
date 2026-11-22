//! Overview, tree, memories and search, one node, the recycle bin.
use super::Web;
use axum::Router;

pub(super) fn routes() -> Router<Web> {
    Router::new()
}

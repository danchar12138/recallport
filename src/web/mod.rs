//! The web pages, served on the loopback address next to the MCP endpoint. This file is the shell they all share:
//! the state, the local-only guard and security headers, the template set and the static files. The pages
//! themselves live in `pages.rs` (overview, tree, memories, node, trash) and `admin.rs` (data, connect).
mod admin;
mod pages;

use crate::{app::App, jobs::Jobs, service::Service};
use anyhow::Result;
use axum::{
    Router,
    body::Body,
    extract::{DefaultBodyLimit, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use minijinja::Environment;
use serde_json::{Value, json};
use std::{net::SocketAddr, sync::Arc};

#[derive(Clone)]
pub(crate) struct Web {
    pub service: Arc<Service>,
    pub base: String,
    pub host: String,
    pub alternate: String,
    pub templates: Arc<Environment<'static>>,
    pub jobs: Arc<Jobs>,
    pub app: Arc<App>,
}

pub fn router(service: Arc<Service>, address: SocketAddr, app: Arc<App>) -> Result<Router> {
    let mut templates = Environment::new();
    templates.add_function("local_time", |value: String, seconds: Option<bool>| {
        chrono::DateTime::parse_from_rfc3339(&value)
            .map(|date| {
                date.with_timezone(&chrono::Local)
                    .format(if seconds.unwrap_or(false) {
                        "%Y-%m-%d %H:%M:%S"
                    } else {
                        "%Y-%m-%d %H:%M"
                    })
                    .to_string()
            })
            .unwrap_or(value)
    });
    // The full set of templates, one file each in `web/`. `page.html` is the frame; the others are what goes in it.
    macro_rules! templates {
        ($($name:literal),+) => {
            $(templates.add_template(concat!($name, ".html"), include_str!(concat!("../../web/", $name, ".html")))?;)+
        };
    }
    templates!(
        "page", "overview", "tree", "memories", "node", "trash", "data", "connect"
    );
    let web = Web {
        service: service.clone(),
        base: format!("http://{address}"),
        host: address.to_string(),
        alternate: format!("localhost:{}", address.port()),
        templates: Arc::new(templates),
        jobs: Arc::new(Jobs::default()),
        app: app.clone(),
    };
    let router = Router::new()
        .route("/health", get(|| async { "ok\n" }))
        .merge(crate::mcp::router(
            service,
            app,
            &web.host,
            &web.alternate,
            &web.base,
        ))
        .merge(pages::routes())
        .merge(admin::routes());
    // Every static file is compiled in; `web/` has no other readers.
    macro_rules! assets {
        ($router:ident: $($name:literal => $mime:literal),+ $(,)?) => {
            $(let $router = $router.route(
                concat!("/", $name),
                get(|| async {
                    ([(header::CONTENT_TYPE, $mime)], include_str!(concat!("../../web/", $name)))
                }),
            );)+
        };
    }
    assets!(router:
        "style.css" => "text/css; charset=utf-8",
        "admin.css" => "text/css; charset=utf-8",
        "icon.svg" => "image/svg+xml",
        "theme.js" => "text/javascript; charset=utf-8",
        "ui.js" => "text/javascript; charset=utf-8",
        "tree.js" => "text/javascript; charset=utf-8",
        "node.js" => "text/javascript; charset=utf-8",
        "data.js" => "text/javascript; charset=utf-8",
        "vendor/flatpickr.min.css" => "text/css; charset=utf-8",
        "vendor/flatpickr.min.js" => "text/javascript; charset=utf-8",
    );
    Ok(router
        .with_state(web.clone())
        .layer(DefaultBodyLimit::max(128 * 1024))
        .layer(middleware::from_fn_with_state(web, local_only)))
}

/// Only this machine, only this origin: requests for another host name (DNS rebinding) or from another site
/// are refused. MCP calls are also noted here, so the connection page can confirm a client really arrived.
async fn local_only(State(w): State<Web>, req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let origin_ok = req
        .headers()
        .get(header::ORIGIN)
        .is_none_or(|v| v.to_str().is_ok_and(|s| s == format!("http://{host}")));
    let site_ok = !req
        .headers()
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| matches!(v, "cross-site" | "same-site"));
    if (host != w.host && host != w.alternate) || !origin_ok || !site_ok {
        return StatusCode::FORBIDDEN.into_response();
    }
    if req.uri().path() == "/mcp" && req.method() == axum::http::Method::POST {
        let (parts, body) = req.into_parts();
        let Ok(body) = axum::body::to_bytes(body, 128 * 1024).await else {
            // A readable JSON-RPC error: the calling model sees it and can retry with a path.
            let reason = "请求体超过 128 KB。附件的 data_base64 请控制在约 96 KB 以内；更大的文件请改用 path（绝对路径），服务会自己读取。";
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                [(header::CONTENT_TYPE, "application/json")],
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":reason}}).to_string(),
            )
                .into_response();
        };
        // The handshake carries the client's name, other requests only the User-Agent.
        let agent = parts
            .headers
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        let name = serde_json::from_slice::<Value>(&body)
            .ok()
            .filter(|v| v["method"] == "initialize")
            .and_then(|v| v["params"]["clientInfo"]["name"].as_str().map(str::to_owned))
            .unwrap_or_default();
        w.app.saw_client(&name, agent);
        return headers(next.run(Request::from_parts(parts, Body::from(body))).await);
    }
    headers(next.run(req).await)
}

fn headers(mut r: Response) -> Response {
    let h = r.headers_mut();
    // Pages get the strict default. Attachment responses bring their own, stricter policy.
    h.entry(header::CONTENT_SECURITY_POLICY).or_insert("default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; media-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'".parse().unwrap());
    h.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    h.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    h.insert(header::REFERRER_POLICY, "same-origin".parse().unwrap());
    r
}

impl Web {
    /// Render a page: `model["View"]` names what goes into the frame (`page.html`).
    pub(crate) fn render(&self, mut model: Value) -> Response {
        model["Base"] = json!(self.base);
        match self
            .templates
            .get_template("page.html")
            .and_then(|t| t.render(model))
        {
            Ok(body) => Html(body).into_response(),
            Err(error) => {
                crate::log!("Template error: {error:#}");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}

/// An error as a short page-level message (the pages show it in a banner).
pub(crate) fn failure(error: &anyhow::Error) -> String {
    format!("{error:#}")
}

pub(crate) fn human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

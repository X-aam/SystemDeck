//! Browser admin dashboard. The files are compiled into the binary and talk
//! to the same JSON API as the desktop client.

use axum::{
    http::{header, HeaderValue},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};

use crate::AppState;

const INDEX: &str = include_str!("web/index.html");
const APP_JS: &str = include_str!("web/app.js");
const APP_CSS: &str = include_str!("web/app.css");
const RRWEB_PLAYER_JS: &str = include_str!("web/rrweb-player.js");
const RRWEB_PLAYER_CSS: &str = include_str!("web/rrweb-player.css");
const REPLAY_HTML: &str = include_str!("web/replay.html");
const REPLAY_JS: &str = include_str!("web/replay.js");
const FAVICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><circle cx="16" cy="16" r="16" fill="#38c2b2"/><path d="M21 10.5c-1-1.6-2.9-2.5-5-2.5-2.8 0-5 1.6-5 4 0 5 10 3 10 8 0 2.4-2.2 4-5 4-2.1 0-4-.9-5-2.5" fill="none" stroke="#fff" stroke-width="3.2" stroke-linecap="round" stroke-linejoin="round"/></svg>"##;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(|| async { asset(INDEX, "text/html; charset=utf-8") }))
        .route("/admin", get(|| async { asset(INDEX, "text/html; charset=utf-8") }))
        .route("/app.js", get(|| async { asset(APP_JS, "text/javascript; charset=utf-8") }))
        .route("/app.css", get(|| async { asset(APP_CSS, "text/css; charset=utf-8") }))
        .route("/favicon.svg", get(|| async { asset(FAVICON, "image/svg+xml") }))
        .route("/favicon.ico", get(|| async { asset(FAVICON, "image/svg+xml") }))
        // Browser-session (rrweb) replay, isolated in an iframe with a looser CSP
        // so recorded inline styles and images render. No data until the parent
        // dashboard posts the events in, so this page carries nothing sensitive.
        .route("/replay", get(|| async { replay_asset(REPLAY_HTML, "text/html; charset=utf-8") }))
        .route("/replay.js", get(|| async { replay_asset(REPLAY_JS, "text/javascript; charset=utf-8") }))
        .route("/rrweb-player.js", get(|| async { asset(RRWEB_PLAYER_JS, "text/javascript; charset=utf-8") }))
        .route("/rrweb-player.css", get(|| async { asset(RRWEB_PLAYER_CSS, "text/css; charset=utf-8") }))
}

/// Replay iframe pages: own scripts only, but recorded inline styles/images
/// must render, so styles and remote images/fonts are allowed here.
fn replay_asset(body: &'static str, content_type: &'static str) -> Response {
    let mut resp = body.into_response();
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src * data: blob:; font-src * data:; media-src * data: blob:; frame-ancestors 'self'; base-uri 'none'; form-action 'none'",
        ),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("SAMEORIGIN"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

fn asset(body: &'static str, content_type: &'static str) -> Response {
    let mut resp = body.into_response();
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    // Only our own files; no inline script, no framing.
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

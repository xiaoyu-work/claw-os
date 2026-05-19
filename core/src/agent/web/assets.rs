//! Static asset serving — the embedded single-page web UI.
//!
//! The HTML page is embedded with [`include_str!`] so the `cos`
//! binary stays a single self-contained file. No node/npm build
//! pipeline, no on-disk asset directory to keep in sync with
//! packaging. Tailwind, Lucide icons and `marked` (Markdown rendering)
//! load over CDN — the alternative is shipping ~300KB of minified JS
//! inside the binary for a UI that is, by design, only used on
//! networked headless boxes.
//!
//! The styling — color tokens, layout, component anatomy — is a
//! direct port of the open-agents web UI in
//! `apps/web/components/{tool-call,assistant-message-groups,
//! thinking-block,inbox-sidebar,session-drawer}`. Only the bits that
//! map to the `cos agent` data model survived the port — there's no
//! GitHub PR / Vercel sandbox / multi-user collaboration in our
//! single-user, localhost-only world.

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

const INDEX_HTML: &str = include_str!("index.html");

pub async fn index() -> Response {
    let mut resp = (StatusCode::OK, INDEX_HTML).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    resp
}

pub async fn favicon() -> Response {
    // A 1x1 transparent PNG. Suppresses the noisy 404 on every page
    // load without paying the cost of designing an actual icon.
    const PIXEL: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    let mut resp = (StatusCode::OK, PIXEL.to_vec()).into_response();
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=86400"),
    );
    resp
}

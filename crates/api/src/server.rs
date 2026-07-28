//! Serves the OpenRPC document and the self-contained interactive
//! reference page — no Swagger UI bundle dependency; the page is plain
//! HTML/JS that fetches `/openrpc.json` and lets a user browse methods
//! and run them live against `/rpc` on the same origin.

use crate::openrpc;
use axum::Router;
use axum::response::Html;
use axum::routing::get;

const REFERENCE_PAGE: &str = include_str!("../assets/reference.html");

pub fn router() -> Router {
    Router::new().route("/openrpc.json", get(handle_openrpc_document)).route("/docs", get(handle_reference_page))
}

async fn handle_openrpc_document() -> axum::Json<serde_json::Value> {
    axum::Json(openrpc::build_document())
}

async fn handle_reference_page() -> Html<&'static str> {
    Html(REFERENCE_PAGE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn openrpc_document_is_served_as_json() {
        let response = router().oneshot(axum::http::Request::builder().uri("/openrpc.json").body(axum::body::Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["openrpc"], serde_json::Value::from("1.2.6"));
    }

    #[tokio::test]
    async fn reference_page_is_served_as_html() {
        let response = router().oneshot(axum::http::Request::builder().uri("/docs").body(axum::body::Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&body).contains("OpenFiat Node API Reference"));
    }
}

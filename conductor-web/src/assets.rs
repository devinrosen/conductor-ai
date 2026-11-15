use axum::http::{header, Method, StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use rust_embed::Embed;

use crate::error::ApiError;

#[derive(Embed)]
#[folder = "frontend/dist/"]
struct Assets;

/// True for `/api` and anything under `/api/`, but not `/apiary` or `/api-docs`.
fn is_api_path(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

pub async fn static_handler(method: Method, uri: Uri) -> Response {
    // An unmatched API path is a client error, not a web UI deep link. Echo the
    // path only (not the query string) so query values aren't reflected back.
    if is_api_path(uri.path()) {
        return ApiError::NotFound(format!("no such API route: {method} {}", uri.path()))
            .into_response();
    }

    let path = uri.path().trim_start_matches('/');

    if let Some(content) = Assets::get(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        (
            [(header::CONTENT_TYPE, mime.as_ref())],
            content.data.into_owned(),
        )
            .into_response()
    } else {
        // SPA fallback: serve index.html for any unmatched non-API path
        match Assets::get("index.html") {
            Some(content) => Html(content.data.into_owned()).into_response(),
            None => (
                StatusCode::SERVICE_UNAVAILABLE,
                Html("<pre>Frontend not built.\nRun: cd conductor-web/frontend &amp;&amp; bun install &amp;&amp; bun run build</pre>"),
            ).into_response(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn body_json(resp: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn is_api_path_cases() {
        assert!(is_api_path("/api"));
        assert!(is_api_path("/api/"));
        assert!(is_api_path("/api/notifications"));
        assert!(!is_api_path("/apiary"));
        assert!(!is_api_path("/api-docs"));
        assert!(!is_api_path("/runs/abc"));
        assert!(!is_api_path("/"));
    }

    #[tokio::test]
    async fn unknown_api_route_returns_json_404() {
        let resp = static_handler(Method::GET, "/api/notifications".parse().unwrap()).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        let body = body_json(resp).await;
        assert_eq!(body["error"], "no such API route: GET /api/notifications");
    }

    #[tokio::test]
    async fn unknown_api_route_names_the_method() {
        let resp = static_handler(Method::POST, "/api/nope?token=secret".parse().unwrap()).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_json(resp).await;
        assert_eq!(body["error"], "no such API route: POST /api/nope");
    }

    #[tokio::test]
    async fn deep_link_still_serves_spa() {
        let resp = static_handler(Method::GET, "/runs/01ABC".parse().unwrap()).await;
        // 200 index.html when the frontend is built, 503 "not built" page otherwise.
        assert_ne!(resp.status(), StatusCode::NOT_FOUND);
        let ct = resp.headers().get(header::CONTENT_TYPE).unwrap();
        assert!(ct.to_str().unwrap().starts_with("text/html"));
    }
}

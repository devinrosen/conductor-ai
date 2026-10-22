use axum::http::{header, StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "frontend/dist/"]
struct Assets;

pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    if let Some(content) = Assets::get(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        (
            [(header::CONTENT_TYPE, mime.as_ref())],
            content.data.into_owned(),
        )
            .into_response()
    } else {
        // SPA fallback: serve index.html for any unmatched path
        match Assets::get("index.html") {
            Some(content) => Html(content.data.into_owned()).into_response(),
            None => (
                StatusCode::SERVICE_UNAVAILABLE,
                Html("<pre>Frontend not built.\nRun: cd conductor-web/frontend &amp;&amp; bun install &amp;&amp; bun run build</pre>"),
            ).into_response(),
        }
    }
}

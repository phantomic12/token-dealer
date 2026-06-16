//! Request-ID middleware. Honors an inbound `x-request-id` header,
//! otherwise generates a UUID. The ID is also set on the response.

use axum::{extract::Request, http::HeaderName, middleware::Next, response::Response};
use tower_http::request_id::{MakeRequestUuid, SetRequestIdLayer};
use uuid::Uuid;

pub const X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

pub async fn request_id_middleware(mut req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get(&X_REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .map(String::from)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    if let Ok(val) = id.parse() {
        req.headers_mut().insert(X_REQUEST_ID.clone(), val);
    }
    let mut resp = next.run(req).await;
    if let Ok(val) = id.parse() {
        resp.headers_mut().insert(X_REQUEST_ID.clone(), val);
    }
    resp
}

pub fn request_id_layer() -> SetRequestIdLayer<MakeRequestUuid> {
    SetRequestIdLayer::new(X_REQUEST_ID.clone(), MakeRequestUuid)
}

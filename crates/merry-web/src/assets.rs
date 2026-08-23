//! Embedded Web application assets.

use axum::{
    http::{HeaderValue, header},
    response::IntoResponse,
};

pub(crate) async fn app_shell() -> impl IntoResponse {
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            (
                header::REFERRER_POLICY,
                HeaderValue::from_static("no-referrer"),
            ),
        ],
        include_str!("../assets/index.html"),
    )
}

pub(crate) async fn app_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/javascript; charset=utf-8"),
        )],
        include_str!("../assets/app.js"),
    )
}

pub(crate) async fn app_contract_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/javascript; charset=utf-8"),
        )],
        include_str!("../assets/trajectory-contract.js"),
    )
}

pub(crate) async fn app_generated_contract_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/javascript; charset=utf-8"),
        )],
        include_str!("../assets/trajectory-contract.generated.js"),
    )
}

pub(crate) async fn app_message_model_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/javascript; charset=utf-8"),
        )],
        include_str!("../assets/trajectory-message-model.js"),
    )
}

pub(crate) async fn app_timeline_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/javascript; charset=utf-8"),
        )],
        include_str!("../assets/trajectory-timeline.js"),
    )
}

pub(crate) async fn app_format_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/javascript; charset=utf-8"),
        )],
        include_str!("../assets/trajectory-format.js"),
    )
}

pub(crate) async fn app_view_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/javascript; charset=utf-8"),
        )],
        include_str!("../assets/trajectory-view.js"),
    )
}

pub(crate) async fn app_css() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/css; charset=utf-8"),
        )],
        include_str!("../assets/app.css"),
    )
}

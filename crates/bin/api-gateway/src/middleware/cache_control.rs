use axum::{
    http::{header::CACHE_CONTROL, HeaderValue},
    response::Response,
};

/// Opt in only for a document whose bytes are independent of the caller and deployment scope.
#[derive(Clone, Copy)]
pub(crate) struct PublicSchema;

pub(crate) async fn apply(mut response: Response) -> Response {
    let policy = if response.extensions_mut().remove::<PublicSchema>().is_some() {
        "public, max-age=300"
    } else {
        "private, no-store"
    };
    let headers = response.headers_mut();
    // Keep a competing CDN directive from overriding the selected policy.
    for name in [
        "cdn-cache-control",
        "vercel-cdn-cache-control",
        "surrogate-control",
    ] {
        headers.remove(name);
    }
    headers.insert(CACHE_CONTROL, HeaderValue::from_static(policy));
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{http::header::ETAG, response::IntoResponse, Extension};

    #[tokio::test]
    async fn scoped_responses_override_cache_headers_and_keep_validators() {
        let response = (
            [
                (CACHE_CONTROL.as_str(), "public, max-age=60"),
                ("cdn-cache-control", "max-age=60"),
                ("vercel-cdn-cache-control", "max-age=60"),
                ("surrogate-control", "max-age=60"),
                (ETAG.as_str(), "\"stable\""),
            ],
            "scoped response",
        )
            .into_response();
        let response = apply(response).await;
        let headers = response.headers();
        assert_eq!(headers[CACHE_CONTROL], "private, no-store");
        assert_eq!(headers[ETAG], "\"stable\"");
        for name in [
            "cdn-cache-control",
            "vercel-cdn-cache-control",
            "surrogate-control",
        ] {
            assert!(!headers.contains_key(name));
        }
    }

    #[tokio::test]
    async fn immutable_schema_can_opt_in_to_public_caching() {
        let response = apply((Extension(PublicSchema), "schema").into_response()).await;
        assert_eq!(response.headers()[CACHE_CONTROL], "public, max-age=300");
        assert!(response.extensions().get::<PublicSchema>().is_none());
    }
}

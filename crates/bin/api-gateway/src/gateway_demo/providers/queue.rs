use reqwest::Response;

pub(super) enum Submission {
    Accepted(String),
    Unavailable(RateLimit),
    Rejected(&'static str),
    Uncertain(&'static str),
}

pub(super) struct RateLimit {
    pub retry_after_ms: Option<i32>,
    pub cooldown_ms: i32,
}

pub(super) enum Observation<T> {
    StillRunning(&'static str),
    Ready(T),
    Rejected(&'static str),
    Uncertain(&'static str),
    Retry,
}

pub(super) enum ResponseError {
    Transport,
    Invalid,
}

pub(super) async fn read_body(
    mut response: Response,
    limit: usize,
) -> Result<Vec<u8>, ResponseError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(ResponseError::Invalid);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ResponseError::Transport)?
    {
        if body.len() + chunk.len() > limit {
            return Err(ResponseError::Invalid);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

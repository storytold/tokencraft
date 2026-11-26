use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use db_queries::gateway_webhooks::{self, Claim, ClaimOutcome, DeliveryResult};
use tokio::task::Id;
use tracing::Instrument;

use crate::{gateway_demo::readiness::Readiness, middleware::trace_context::work_span};

use super::config::WebhookConfig;

const MAX_IN_FLIGHT: usize = 4;
const MAX_CLAIMS_PER_TICK: usize = 16;
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

pub(in crate::gateway_demo) fn spawn(pool: db::PgPool, config: Arc<WebhookConfig>, readiness: Arc<Readiness>) {
    tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        let mut in_flight = HashMap::new();
        let groups = config.destinations_by_origin();
        let mut timer = tokio::time::interval(Duration::from_millis(250));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            while let Some(result) = tasks.try_join_next_with_id() {
                let task_id = match result {
                    Ok((id, ())) => id,
                    Err(error) => {
                        tracing::warn!("webhook task stopped before writing its outcome");
                        error.id()
                    }
                };
                in_flight.remove(&task_id);
            }
            for _ in 0..MAX_CLAIMS_PER_TICK {
                if tasks.len() >= MAX_IN_FLIGHT || !readiness.database_schema_ready() {
                    break;
                }
                let excluded = saturated_destinations(&groups, &in_flight);
                match gateway_webhooks::claim(&pool, excluded).await {
                    Ok(ClaimOutcome::Claimed(claim)) => {
                        let origin = config
                            .matches_destination(&claim.destination_id, &claim.destination_url)
                            .map(|url| url.origin().ascii_serialization());
                        let task = tasks.spawn(deliver(pool.clone(), Arc::clone(&config), claim));
                        if let Some(origin) = origin {
                            in_flight.insert(task.id(), origin);
                        }
                    }
                    Ok(ClaimOutcome::Closed) => continue,
                    Ok(ClaimOutcome::Idle) => break,
                    Err(error) => {
                        log_database_error(&error);
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        break;
                    }
                }
            }
        }
    });
}

fn saturated_destinations<'a>(
    groups: &'a HashMap<String, Vec<String>>,
    in_flight: &HashMap<Id, String>,
) -> &'a [String] {
    // With multiple configured origins, keep the last slot for a different
    // receiver when one origin already owns the other three.
    if groups.len() < 2 || in_flight.len() != MAX_IN_FLIGHT - 1 {
        return &[];
    }
    let origin = in_flight.values().next().expect("three active deliveries");
    if in_flight.values().all(|value| value == origin) {
        groups.get(origin).map_or(&[], Vec::as_slice)
    } else {
        &[]
    }
}

async fn deliver(pool: db::PgPool, config: Arc<WebhookConfig>, claim: Claim) {
    let span = if claim.is_workflow {
        tracing::info_span!("workflow_webhook_delivery", workflow_run_id = %claim.subject_id)
    } else {
        work_span(
            "webhook_delivery", &claim.subject_id, claim.trace_id.as_deref(), claim.admission_span_id.as_deref(),
        )
    };
    async {
        let started = Instant::now();
        let result = send(&config, &claim).await;
        match gateway_webhooks::finish(&pool, &claim, result).await {
            Ok(true) => tracing::debug!(
                elapsed_ms = started.elapsed().as_millis() as u64,
                attempt = claim.attempt,
                "webhook delivery cycle finished"
            ),
            Ok(false) => tracing::debug!(
                attempt = claim.attempt,
                "webhook result ignored after lease expired"
            ),
            Err(error) => log_database_error(&error),
        }
    }.instrument(span).await;
}

async fn send(config: &WebhookConfig, claim: &Claim) -> DeliveryResult {
    let Some(url) = config.matches_destination(&claim.destination_id, &claim.destination_url)
    else {
        return DeliveryResult::DestinationUnavailable;
    };
    let timestamp = chrono::Utc::now().timestamp().to_string();
    let response = config
        .client
        .post(url.clone())
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header("webhook-id", &claim.id)
        .header("webhook-timestamp", &timestamp)
        .header("webhook-signature", config.signature(claim, &timestamp))
        .body(claim.payload.clone())
        .send()
        .await;
    let backoff = 1_000 * (1 << (claim.tries_in_cycle - 1)) + rand::random::<i32>().rem_euclid(250);
    let Ok(response) = response else {
        return DeliveryResult::NetworkError {
            retry_after_ms: backoff,
        };
    };
    let status = response.status();
    let code = status.as_u16() as i16;
    if status.is_success() {
        return DeliveryResult::Delivered(code);
    }
    let retry_after_ms = (status.is_server_error() || matches!(code, 408 | 425 | 429)).then(|| {
        let requested = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|header| header.to_str().ok())
            .map_or(0, |value| retry_after_ms(value, SystemTime::now()));
        backoff.max(requested)
    });
    DeliveryResult::HttpError {
        status: code,
        retry_after_ms,
    }
}

fn retry_after_ms(value: &str, now: SystemTime) -> i32 {
    let value = value.trim();
    let delay = match value.parse::<u64>() {
        Ok(seconds) => Duration::from_secs(seconds),
        Err(_) => httpdate::parse_http_date(value)
            .ok()
            .and_then(|date| date.duration_since(now).ok())
            .unwrap_or_default(),
    };
    delay.min(MAX_RETRY_AFTER).as_nanos().div_ceil(1_000_000) as i32
}

fn log_database_error(error: &sqlx::Error) {
    tracing::warn!(sqlstate = ?error.as_database_error().and_then(|error| error.code()), "webhook worker database operation failed");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_both_retry_after_formats_and_bounds_the_delay() {
        let now = httpdate::parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT").unwrap();
        assert_eq!(retry_after_ms(" 4 ", now), 4_000);
        assert_eq!(retry_after_ms("Sun, 06 Nov 1994 08:49:42 GMT", now), 5_000);
        assert_eq!(retry_after_ms("Sun, 06 Nov 1994 08:49:36 GMT", now), 0);
        assert_eq!(retry_after_ms("Sun, 06 Nov 1994 09:00:00 GMT", now), 60_000);
        assert_eq!(retry_after_ms("18446744073709551615", now), 60_000);
        assert_eq!(retry_after_ms("invalid", now), 0);
    }
}

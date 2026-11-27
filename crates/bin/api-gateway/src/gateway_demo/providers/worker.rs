use std::{
    borrow::Cow,
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use db_queries::gateway_requests::{
    self,
    queued_runs::{self, ClaimedQueuePoll, PollBindings},
    ClaimedProviderRun, SubmitBindings,
};
use tokio::{task::{Id, JoinSet}, time::MissedTickBehavior};
use tracing::Instrument;

use crate::{
    gateway_demo::{catalog_refresh::CatalogStore, readiness::Readiness},
    middleware::trace_context::work_span,
};

use super::{dispatch, log_database_error, observe_queue, ProviderRuntime, MAX_PROVIDER_TASKS};

pub(super) async fn run(
    pool: db::PgPool,
    runtime: Arc<ProviderRuntime>,
    catalog: Arc<CatalogStore>,
    readiness: Arc<Readiness>,
) {
    let chat: Vec<_> = runtime.local_chat.values().map(|entry| entry.digest.clone()).collect();
    let submit_connectors: Vec<_> = runtime.connectors.active_digests().map(str::to_owned).collect();
    let poll_connectors: Vec<_> = runtime.connectors.digests().map(str::to_owned).collect();
    let recovery: Vec<_> = runtime.connectors.recovery_digests().map(str::to_owned).collect();
    let destinations = runtime.connectors.worker_destinations()
        .chain(runtime.local_chat.values().map(|binding| {
            let destination = binding.client.worker_origin().unwrap_or_else(|| binding.digest.clone());
            (binding.digest.as_str(), destination)
        }));
    let reserve_chat = !chat.is_empty() && runtime.artifact_storage.is_some();
    let mut slots = WorkerSlots::new(destinations, reserve_chat);
    let mut tasks = JoinSet::new();
    let mut timer = tokio::time::interval(Duration::from_millis(250));
    timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut prefer_poll = true;

    loop {
        timer.tick().await;
        while let Some(result) = tasks.try_join_next_with_id() {
            let id = match result {
                Ok((id, ())) => id,
                Err(error) => {
                    tracing::warn!("provider task stopped before recording its result");
                    error.id()
                }
            };
            slots.active.remove(&id);
        }
        readiness.set_provider_tasks(tasks.len());
        if tasks.len() >= MAX_PROVIDER_TASKS {
            readiness.provider_observed(true);
            continue;
        }

        for _ in tasks.len()..MAX_PROVIDER_TASKS {
            if !readiness.database_schema_ready() {
                break;
            }
            let blocked = slots.blocked_destination();
            let local_chat = slots.eligible(&chat, blocked);
            let connectors = slots.eligible_connectors(&submit_connectors, blocked);
            let polling = slots.eligible_connectors(&poll_connectors, blocked);
            let submit = SubmitBindings {
                local_chat: &local_chat,
                connectors: &connectors,
                connector_recovery: &recovery,
            };
            let work = claim_work(&pool, submit, &polling, prefer_poll).await;
            readiness.provider_observed(work.is_ok());
            match work {
                Ok(Some(work)) => {
                    // Give the other queue first choice on the next claim.
                    prefer_poll = matches!(&work, Work::Submit(_));
                    let active = ActiveTask {
                        destination: slots.bindings[work.binding_digest()].clone(),
                        connector: work.uses_connector(),
                    };
                    let task = tasks.spawn(work.execute(
                        pool.clone(),
                        Arc::clone(&runtime),
                        Arc::clone(&catalog),
                    ));
                    slots.active.insert(task.id(), active);
                    readiness.set_provider_tasks(tasks.len());
                }
                Ok(None) => break,
                Err(error) => {
                    log_database_error(&error);
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    break;
                }
            }
        }
    }
}

struct ActiveTask {
    destination: String,
    connector: bool,
}

struct WorkerSlots<'a> {
    bindings: HashMap<&'a str, String>,
    active: HashMap<Id, ActiveTask>,
    destination_limit: usize,
    reserve_chat: bool,
}

impl<'a> WorkerSlots<'a> {
    fn new(destinations: impl Iterator<Item = (&'a str, String)>, reserve_chat: bool) -> Self {
        let bindings: HashMap<_, _> = destinations.collect();
        let multiple = bindings
            .values()
            .next()
            .is_some_and(|first| bindings.values().any(|origin| origin != first));
        // Leave one slot for other destinations. A dedicated worker can use all four.
        let destination_limit = if multiple {
            MAX_PROVIDER_TASKS - 1
        } else {
            MAX_PROVIDER_TASKS
        };
        Self {
            bindings,
            active: HashMap::new(),
            destination_limit,
            reserve_chat,
        }
    }

    fn blocked_destination(&self) -> Option<&str> {
        // With four total slots and a limit of three, at most one destination is full.
        self.active
            .values()
            .find(|task| {
                self.active
                    .values()
                    .filter(|active| active.destination == task.destination)
                    .count()
                    >= self.destination_limit
            })
            .map(|task| task.destination.as_str())
    }

    fn eligible_connectors<'b>(
        &self,
        bindings: &'b [String],
        blocked: Option<&str>,
    ) -> Cow<'b, [String]> {
        // Separate connector origins can share the same slow artifact store.
        if self.reserve_chat
            && self.active.values().filter(|task| task.connector).count() >= MAX_PROVIDER_TASKS - 1
        {
            return Cow::Borrowed(&[]);
        }
        self.eligible(bindings, blocked)
    }

    fn eligible<'b>(&self, bindings: &'b [String], blocked: Option<&str>) -> Cow<'b, [String]> {
        match blocked {
            Some(destination) => Cow::Owned(bindings.iter()
                .filter(|binding| self.bindings[binding.as_str()] != destination)
                .cloned()
                .collect()),
            None => Cow::Borrowed(bindings),
        }
    }
}

enum Work {
    Submit(ClaimedProviderRun),
    Poll(ClaimedQueuePoll),
}

async fn claim_work(
    pool: &db::PgPool,
    submit: SubmitBindings<'_>,
    poll_connectors: &[String],
    prefer_poll: bool,
) -> sqlx::Result<Option<Work>> {
    for poll in [prefer_poll, !prefer_poll] {
        let task = if poll {
            if poll_connectors.is_empty() {
                continue;
            }
            queued_runs::claim_queue_poll(pool, PollBindings { connectors: poll_connectors }).await?.map(Work::Poll)
        } else if !submit.local_chat.is_empty() || !submit.connectors.is_empty() {
            gateway_requests::claim_provider_run(pool, submit).await?.map(Work::Submit)
        } else {
            continue;
        };
        if task.is_some() {
            return Ok(task);
        }
    }
    Ok(None)
}

impl Work {
    fn binding_digest(&self) -> &str {
        match self {
            Self::Submit(run) => &run.binding_digest,
            Self::Poll(run) => &run.binding_digest,
        }
    }

    fn uses_connector(&self) -> bool {
        match self {
            Self::Submit(run) => run.adapter == "connector_queue",
            Self::Poll(_) => true,
        }
    }

    async fn execute(self, pool: db::PgPool, runtime: Arc<ProviderRuntime>, catalog: Arc<CatalogStore>) {
        let span = match &self {
            Self::Submit(run) => work_span(
                "provider_submit", &run.request_id, run.trace_id.as_deref(), run.admission_span_id.as_deref(),
            ),
            Self::Poll(run) => work_span(
                "provider_poll", &run.request_id, run.trace_id.as_deref(), run.admission_span_id.as_deref(),
            ),
        };
        async {
            let started = Instant::now();
            let result = match self {
                Self::Submit(run) => dispatch(&pool, &runtime, &catalog.current(), run).await,
                Self::Poll(task) => observe_queue(&pool, &runtime, task).await,
            };
            if let Err(error) = result {
                log_database_error(&error);
            } else {
                tracing::debug!(elapsed_ms = started.elapsed().as_millis() as u64, "provider worker cycle finished");
            }
        }.instrument(span).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn aliases_share_capacity_and_a_dedicated_destination_gets_all_slots() {
        let bindings = ["primary", "previous", "alternate"].map(str::to_owned);
        for multiple_destinations in [true, false] {
            let alternate = if multiple_destinations {
                "alternate-origin"
            } else {
                "shared-origin"
            };
            let mut slots = WorkerSlots::new(
                [
                    ("primary", "shared-origin".to_owned()),
                    ("previous", "shared-origin".to_owned()),
                    ("alternate", alternate.to_owned()),
                ]
                .into_iter(),
                false,
            );
            let mut tasks = JoinSet::new();
            for binding in ["primary", "previous", "primary"] {
                let task = tasks.spawn(std::future::pending::<()>());
                slots.active.insert(
                    task.id(),
                    ActiveTask {
                        destination: slots.bindings[binding].clone(),
                        connector: true,
                    },
                );
            }
            let eligible = slots.eligible(&bindings, slots.blocked_destination());
            if multiple_destinations {
                assert_eq!(eligible.as_ref(), &bindings[2..]);
            } else {
                assert_eq!(eligible.as_ref(), &bindings);
            }

            tasks.abort_all();
            let interrupted = tasks.join_next_with_id().await.unwrap().unwrap_err();
            slots.active.remove(&interrupted.id());
            assert!(slots.blocked_destination().is_none());
        }
    }

    #[tokio::test]
    async fn connectors_share_the_budget_that_protects_chat_from_storage_stalls() {
        let bindings = ["one", "two", "chat"].map(str::to_owned);
        let destinations = [
            ("one", "origin-a".to_owned()),
            ("two", "origin-b".to_owned()),
            ("chat", "origin-c".to_owned()),
        ];
        let mut tasks = JoinSet::new();
        let mut slots = WorkerSlots::new(destinations.into_iter(), true);
        let mut ids = Vec::new();
        for binding in ["one", "two", "two"] {
            let task = tasks.spawn(std::future::pending::<()>());
            slots.active.insert(
                task.id(),
                ActiveTask {
                    destination: slots.bindings[binding].clone(),
                    connector: true,
                },
            );
            ids.push(task.id());
        }

        assert!(slots.blocked_destination().is_none());
        assert!(slots.eligible_connectors(&bindings[..2], None).is_empty());
        assert_eq!(
            slots.eligible(&bindings[2..], None).as_ref(),
            &bindings[2..]
        );

        slots.active.remove(&ids[0]);
        assert_eq!(
            slots.eligible_connectors(&bindings[..2], None).as_ref(),
            &bindings[..2]
        );
    }
}

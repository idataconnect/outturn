use std::sync::Arc;

use outturn_core::jobs;

use super::{CHAT_TURN, ChatTurnPayload, Worker};

impl Worker {
    /// Returns work whose runtime stopped reporting.
    ///
    /// A turn is claimed by the tier handing it out and reported by the
    /// runtime running it, and nothing connects those two but a lease. When a
    /// runtime is killed mid-turn nobody fails the job -- the pod that would
    /// have is gone -- so without this the row stays `running` for ever, and
    /// because a serial key admits no second job while one is running, every
    /// later turn in that session is blocked behind it.
    pub fn spawn_reaper(self: Arc<Self>, shutdown: outturn_core::lifecycle::Shutdown) {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(jobs::LEASE_HEARTBEAT);
            let ttl_days = crate::storage_sweep::ttl_days();
            let mut last_storage_sweep: Option<std::time::Instant> = None;
            loop {
                tokio::select! {
                    _ = shutdown.wait() => return,
                    _ = ticker.tick() => {
                        match jobs::reap_abandoned(&self.pool).await {
                            Ok((0, _)) => {}
                            Ok((n, gave_up)) => {
                                tracing::info!(jobs = n, "returned abandoned work to the queue");
                                // A job the reaper parks as failed has no
                                // worker left to clean up after it. Left
                                // alone, its empty reply blocks the session
                                // and the reader waits for ever.
                                for job in gave_up {
                                    if job.kind != CHAT_TURN {
                                        continue;
                                    }
                                    match serde_json::from_value::<ChatTurnPayload>(job.payload) {
                                        Ok(payload) => {
                                            self.abandon_payload(
                                                &payload,
                                                "the turn was lost too many times and was given up on",
                                            )
                                            .await
                                        }
                                        Err(e) => tracing::error!(job_id = %job.id, error = %e, "payload"),
                                    }
                                }
                            }
                            Err(e) => tracing::error!(error = %e, "could not reap abandoned work"),
                        }

                        // Keeps the live-session table the size of the
                        // conversations happening now. Nothing depends on it
                        // being prompt -- the count filters on expiry anyway --
                        // so a missed sweep costs a slightly larger scan.
                        let _ = sqlx::query("delete from live_sessions where expires_at < now()")
                            .execute(&self.pool)
                            .await;

                        // `SessionStore::sweep_expired` has existed since the
                        // first migration and was called from nowhere, so
                        // refresh tokens accumulated for ever -- rotated and
                        // revoked rows included, since those are kept until
                        // expiry so a replay stays detectable.
                        //
                        // Through the store rather than as another statement
                        // here: the retention rule is written down in
                        // `sweep_expired`, beside the comment explaining why
                        // revoked rows are kept, and a copy of the statement
                        // in this tick would be the one that runs while the
                        // documented one quietly became dead.
                        {
                            use crate::session::SessionStore as _;
                            let store = crate::session::PostgresSessionStore::new(
                                self.pool.clone(),
                            );
                            let _ = store.sweep_expired().await;
                        }

                        // Deliveries are remembered only for as long as their
                        // signature would still be accepted, so a row past
                        // that protects nothing. Swept here rather than by a
                        // loop of its own: the table holds a few minutes of
                        // traffic by construction, and a missed sweep costs a
                        // slightly larger index rather than a wrong answer --
                        // the primary key is what refuses a replay, not the
                        // expiry.
                        let _ =
                            crate::webhook::postgres::forget_expired(&self.pool).await;

                        // The breaker keeps a sighting per model call it could
                        // not classify, and reads only the recent ones. Nothing
                        // called this before, so the table grew with every such
                        // call ever made. Here because the API's tick is the one
                        // thing that already sweeps tables on a schedule; the
                        // gateway shares the database.
                        let _ =
                            outturn_core::breaker::forget_stale_sightings(&self.pool).await;

                        // Files nobody will read again: idle sessions' and
                        // deleted things'. Hourly, since the window is days
                        // and each pass lists storage. Every API replica runs
                        // it; two passes over one prefix delete it once and
                        // find nothing the second time, which costs a listing.
                        if let Some(storage) = &self.storage
                            && last_storage_sweep
                                .is_none_or(|at| at.elapsed() >= std::time::Duration::from_secs(3600))
                        {
                            last_storage_sweep = Some(std::time::Instant::now());
                            crate::storage_sweep::sweep(&self.pool, storage, ttl_days).await;
                        }

                        // The model catalog, when its week is up. Here for the
                        // same reason as the sweeps: the tick is already the
                        // thing that runs on a schedule. Off the turn path --
                        // a fetch that hangs holds up a sweep, never a reply.
                        if let (Some(minter), Some(gateway)) = (&self.minter, &self.gateway_url) {
                            crate::catalog::check_if_due(&self.pool, minter, gateway).await;
                        }

                        // What finished turns streamed: the deltas, and the
                        // reasoning, calls and steers beside them. Written down
                        // at `chat::forget_streamed`, with why a turn still
                        // streaming keeps its own.
                        let _ = crate::chat::forget_streamed(&self.pool).await;

                        // Rows nothing deleted once they stopped mattering:
                        // declared writes past their day, trigger firings past
                        // their thirty, and catalogs no model names. Each sweep
                        // is written beside the rule it keeps.
                        let _ = crate::invocations::forget_expired(&self.pool).await;
                        let _ = crate::trigger::forget_old_firings(&self.pool).await;
                        let _ = crate::catalog::forget_old_snapshots(&self.pool).await;
                    },
                }
            }
        });
    }
}

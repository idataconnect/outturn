//! Turning uploaded documents into text an agent can read.
//!
//! A PDF is bytes an agent cannot do anything with. Tika reads it and returns
//! words, and those words are stored beside the object so a later read finds
//! them without asking anyone again.
//!
//! Done as background work rather than during the upload. A scanned document
//! can take minutes, and a request held open for it fails in a way the person
//! uploading cannot act on -- where a job that is still running is just a file
//! not ready yet. It also means extraction happens wherever bytes land: an
//! agent writing a document into the workspace gets the same treatment as a
//! person dragging one into the browser, because both enqueue the same job.
//!
//! Absent configuration nothing is attempted. `OUTTURN_TIKA_URL` unset means
//! no jobs are enqueued and no text is written, which is what a deployment
//! that never uploads a document should pay for the feature.

use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use crate::jobs;

pub const EXTRACT: &str = "document.extract";

/// Extracted text sits under its own prefix, mirroring the object's key.
///
/// Outside the scope prefixes on purpose: a listing walks `sessions/...` or
/// `workspaces/...`, so nothing here appears beside the document it came from.
/// An agent asked for one file and should not be shown two.
pub fn text_key(object_key: &str) -> String {
    format!("extracted/{object_key}")
}

/// Whether this is worth handing to Tika.
///
/// By extension rather than by sniffing the bytes: the name is what the person
/// uploading chose, and a wrong guess costs a job that finds nothing rather
/// than a file that silently reads as empty. Plain text needs no extraction --
/// it is already the thing extraction produces.
pub fn is_extractable(path: &str) -> bool {
    const KNOWN: &[&str] = &[
        "pdf", "doc", "docx", "odt", "rtf", "xls", "xlsx", "ods", "ppt", "pptx", "odp", "epub",
        "msg", "eml",
    ];
    path.rsplit('.')
        .next()
        .map(|ext| KNOWN.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ExtractPayload {
    /// The object's real key, already resolved. The job runs long after the
    /// request that made it, and re-resolving would depend on a session that
    /// may be gone.
    pub key: String,
    /// What the person called it, for the log.
    pub path: String,
}

/// Where documents are sent, if anywhere.
pub fn tika_url() -> Option<String> {
    std::env::var("OUTTURN_TIKA_URL")
        .ok()
        .map(|u| u.trim_end_matches('/').to_string())
        .filter(|u| !u.is_empty())
}

/// Queues a document for extraction, if extraction is configured at all.
pub async fn enqueue<'e, E>(executor: E, workspace_id: Uuid, key: &str, path: &str)
where
    E: sqlx::PgExecutor<'e>,
{
    if tika_url().is_none() || !is_extractable(path) {
        return;
    }
    let payload = serde_json::json!({ "key": key, "path": path });
    // Serialised on the key so a file replaced twice in quick succession is
    // read once per write and never by two workers at once, which would race
    // to store different text for the same object.
    if let Err(e) = jobs::enqueue(
        executor,
        workspace_id,
        EXTRACT,
        payload,
        None,
        Some(&format!("extract:{key}")),
        jobs::PRIORITY_BACKGROUND,
    )
    .await
    {
        // Not fatal to the upload. The bytes are stored; only the reading of
        // them failed to be scheduled.
        tracing::warn!(error = %e, path = %path, "could not queue document for extraction");
    }
}

/// Claims extraction work and does it, until shutdown.
///
/// Runs in the API tier because that is where the bucket and the database
/// already are. The runtime deliberately cannot reach a service inside the
/// cluster, and this is not work a turn is waiting on.
pub fn spawn(
    pool: sqlx::PgPool,
    storage: Arc<dyn crate::runtime::storage::StorageBackend>,
    shutdown: Arc<tokio::sync::Notify>,
) {
    let Some(url) = tika_url() else {
        tracing::info!("no OUTTURN_TIKA_URL; documents will not be extracted");
        return;
    };
    tracing::info!(tika = %url, "document extraction enabled");

    tokio::spawn(async move {
        let client = reqwest::Client::new();
        let mut ticker = tokio::time::interval(Duration::from_secs(2));
        loop {
            tokio::select! {
                _ = shutdown.notified() => return,
                _ = ticker.tick() => {
                    let claimed = match jobs::claim(&pool, &[EXTRACT], 1, jobs::DEFAULT_LEASE).await {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::warn!(error = %e, "could not claim extraction work");
                            continue;
                        }
                    };
                    for handle in claimed {
                        let job = handle.job;
                        let outcome = run_one(&client, &url, storage.as_ref(), &job).await;
                        let done = match outcome {
                            Ok(bytes) => {
                                tracing::info!(job_id = %job.id, bytes, "document extracted");
                                jobs::complete(&pool, job.id, job.lease_token).await
                            }
                            Err(e) => {
                                // Retried with the queue's own backoff: Tika
                                // restarting is the ordinary case, and the
                                // bytes are still there to try again.
                                tracing::warn!(job_id = %job.id, error = %e, "extraction failed");
                                jobs::fail(
                                    &pool,
                                    job.id,
                                    &e,
                                    Duration::from_secs(30),
                                    job.lease_token,
                                )
                                .await
                            }
                        };
                        if let Err(e) = done {
                            tracing::warn!(job_id = %job.id, error = %e, "could not close out extraction");
                        }
                    }
                }
            }
        }
    });
}

async fn run_one(
    client: &reqwest::Client,
    url: &str,
    storage: &dyn crate::runtime::storage::StorageBackend,
    job: &jobs::Job,
) -> Result<usize, String> {
    let payload: ExtractPayload =
        serde_json::from_value(job.payload.clone()).map_err(|e| e.to_string())?;

    let bytes = storage
        .read(&payload.key, 0, u32::MAX)
        .await
        .map_err(|e| format!("reading {}: {e}", payload.path))?;

    // Tika's plain-text endpoint. `Accept: text/plain` is what distinguishes it
    // from the metadata endpoints on the same path.
    let response = client
        .put(format!("{url}/tika"))
        .header("Accept", "text/plain")
        .body(bytes)
        .send()
        .await
        .map_err(|e| format!("reaching tika: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("tika answered {}", response.status()));
    }
    let text = response.text().await.map_err(|e| e.to_string())?;
    let trimmed = text.trim();

    // Tika answers 200 with an empty body for a scan with no text layer, or for
    // bytes that are not the document they claim to be. That is a fact about
    // the file, not a failure to retry: the answer will be the same next time.
    // It is written down as an empty object so a reader gets told, rather than
    // finding nothing there and being asked to come back to a file that will
    // never be ready.
    let key = text_key(&payload.key);
    storage
        .write(&key, 0, trimmed.as_bytes())
        .await
        .map_err(|e| format!("storing text: {e}"))?;
    Ok(trimmed.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_are_extractable_and_text_is_not() {
        assert!(is_extractable("report.pdf"));
        assert!(is_extractable("Notes.DOCX"), "extensions are not case-sensitive");
        assert!(is_extractable("deck.pptx"));
        // Already words: extracting it would produce what it already is.
        assert!(!is_extractable("notes.md"));
        assert!(!is_extractable("data.csv"));
        assert!(!is_extractable("photo.png"));
        assert!(!is_extractable("no-extension"));
    }

    /// Text lives outside the scope prefixes, so a listing never shows it.
    #[test]
    fn extracted_text_sits_outside_the_listing() {
        let key = "sessions/w/a/s/report.pdf";
        let text = text_key(key);
        assert_eq!(text, "extracted/sessions/w/a/s/report.pdf");
        assert!(!text.starts_with("sessions/"), "a listing would show it: {text}");
    }
}

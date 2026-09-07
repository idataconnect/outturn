use std::sync::Arc;

use axum::Router;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use outturn::lifecycle::{self, Health};
use outturn::runtime::component::AgentRunner;
use outturn::runtime::router::RuntimeState;
use outturn::runtime::storage::{MemoryStorage, S3Storage, StorageBackend};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let health = Health::new();

    // Read once at startup: it is the same bytes for every session, and
    // compiling it per turn would be waste.
    let agent_module = std::fs::read(
        std::env::var("OUTTURN_AGENT_MODULE")
            .unwrap_or_else(|_| "/usr/local/share/outturn/agent_default.wasm".into()),
    )
    .expect("agent component");

    // The runtime holds no signing key: it presents a shared key that means
    // only "the runtime tier", and the API mints everything a turn needs.
    let runtime_key = std::env::var("OUTTURN_RUNTIME_KEY").expect("OUTTURN_RUNTIME_KEY not set");

    // One bucket, partitioned by tenant prefix. Buckets are a limited
    // resource -- a hundred per AWS account by default -- and a limit on
    // buckets would become a limit on customers.
    let storage: Option<Arc<dyn StorageBackend>> = match std::env::var("OUTTURN_S3_ENDPOINT") {
        Ok(endpoint) => {
            let bucket = std::env::var("OUTTURN_S3_BUCKET").unwrap_or_else(|_| "outturn".into());
            match S3Storage::new(
                &endpoint,
                &bucket,
                &std::env::var("OUTTURN_S3_ACCESS_KEY").unwrap_or_default(),
                &std::env::var("OUTTURN_S3_SECRET_KEY").unwrap_or_default(),
                String::new(),
            ) {
                Ok(store) => {
                    // Refuse to start rather than run without storage: a pod
                    // that comes up with a bucket it cannot reach hands every
                    // agent an error dressed up as a listing. But wait for it
                    // first -- on a fresh cluster this pod is usually up before
                    // the store is, and a dependency arriving is not a fault.
                    let mut attempt = 0u32;
                    loop {
                        match store.ensure_bucket().await {
                            Ok(()) => break,
                            Err(e) if attempt < 60 => {
                                attempt += 1;
                                if attempt == 1 {
                                    tracing::info!(error = %e, "waiting for object storage");
                                }
                                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                            }
                            Err(e) => panic!("object storage bucket: {e}"),
                        }
                    }
                    // Session files are scratch and are swept. How soon is
                    // the operator's call; the runtime holds no database, so
                    // it is an environment variable rather than a setting.
                    let days = std::env::var("OUTTURN_SESSION_FILE_TTL_DAYS")
                        .ok()
                        .and_then(|d| d.parse().ok())
                        .unwrap_or(30);
                    if let Err(e) = store.ensure_session_lifecycle(days).await {
                        tracing::warn!(error = %e, "could not set the session file lifecycle; session files will not be swept");
                    }
                    tracing::info!(endpoint, bucket, "object storage enabled");
                    Some(Arc::new(store))
                }
                Err(e) => {
                    tracing::error!(error = %e, "object storage misconfigured, agents get none");
                    None
                }
            }
        }
        Err(_) => {
            // In-memory rather than nothing, so a local run without MinIO
            // still exercises the same code path -- and loses everything on
            // restart, which is the honest behaviour for a store that is not
            // configured.
            tracing::warn!("no OUTTURN_S3_ENDPOINT; using in-memory object storage");
            Some(Arc::new(MemoryStorage::new()))
        }
    };

    let admission = Arc::new(outturn::runtime::admission::Admission::from_env());
    let state = Arc::new(RuntimeState {
        gateway_url: std::env::var("OUTTURN_GATEWAY_URL")
            .unwrap_or_else(|_| "http://outturn-gateway:8081".into()),
        runner: Arc::new(AgentRunner::new().expect("agent runner")),
        agent_module: Arc::new(agent_module),
        storage,
        admission: Arc::clone(&admission),
    });

    // The runtime asks for work rather than waiting to be handed it, so a pod
    // with no room simply does not ask and is never offered a turn it would
    // have to refuse.
    Arc::new(outturn::runtime::puller::Puller {
        api_url: std::env::var("OUTTURN_API_URL")
            .unwrap_or_else(|_| "http://outturn-api:8080".into()),
        runtime_key: runtime_key.trim().to_string(),
        http: outturn::http_client::streaming_client(outturn::http_client::IDLE_TIMEOUT),
        runner: Arc::clone(&state.runner),
        agent_module: Arc::clone(&state.agent_module),
        storage: state.storage.clone(),
        gateway_url: state.gateway_url.clone(),
        admission: Arc::clone(&state.admission),
        default_model: std::env::var("OUTTURN_DEFAULT_MODEL")
            .unwrap_or_else(|_| "llama3.1".into()),
        idle_timeout: outturn::http_client::IDLE_TIMEOUT,
    })
    .spawn(health.shutdown_signal());

    let app = Router::new()
        .merge(lifecycle::routes(health.clone()));

    let addr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8082".into());
    let listener = TcpListener::bind(&addr).await.unwrap();
    tracing::info!("listening on {addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(lifecycle::shutdown_signal(health))
        .await
        .unwrap();

    // The puller has stopped asking, but turns it already took are still
    // streaming. Leaving now would kill them mid-reply and leave each one to
    // be recovered by its lease -- forty-five seconds of nothing, then the
    // reply starting over on another pod. So wait for them, up to the grace
    // the deployment allows (terminationGracePeriodSeconds), which is what
    // decides whether this wait finishes or is cut off.
    let draining = admission.in_flight();
    if draining > 0 {
        tracing::info!(turns = draining, "waiting for turns in flight to finish");
        while admission.in_flight() > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }
    tracing::info!("shutdown complete");
}

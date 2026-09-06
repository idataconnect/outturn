use std::sync::Arc;

use axum::Router;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use outturn::auth::{TokenMinter, TokenValidator};
use outturn::lifecycle::{self, Health};
use outturn::runtime::component::AgentRunner;
use outturn::runtime::router::{self, RuntimeState};
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

    let validator = TokenValidator::from_env().expect("token validator");
    let (minter, _public) = TokenMinter::from_env().expect("token minter");

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

    let state = Arc::new(RuntimeState {
        auth: validator,
        minter,
        gateway_url: std::env::var("OUTTURN_GATEWAY_URL")
            .unwrap_or_else(|_| "http://outturn-gateway:8081".into()),
        runner: Arc::new(AgentRunner::new().expect("agent runner")),
        agent_module: Arc::new(agent_module),
        storage,
        admission: Arc::new(outturn::runtime::admission::Admission::from_env()),
    });

    // The runtime asks for work rather than waiting to be handed it, so a pod
    // with no room simply does not ask and is never offered a turn it would
    // have to refuse.
    Arc::new(outturn::runtime::puller::Puller {
        api_url: std::env::var("OUTTURN_API_URL")
            .unwrap_or_else(|_| "http://outturn-api:8080".into()),
        token: state
            .minter
            .mint(uuid::Uuid::now_v7(), uuid::Uuid::nil(), &[outturn::auth::Role::Operator])
            .expect("work token"),
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
    tracing::info!("shutdown complete");
}

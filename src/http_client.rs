//! Outbound HTTP clients, with deadlines.
//!
//! TCP already handles a peer that dies: keepalive is on by default and a
//! dead connection errors within about a minute, after which the job retries.
//! What TCP cannot see is a peer that is alive and silent -- a stalled model,
//! a proxy holding a response it will never finish. The socket is healthy,
//! probes are answered, and nothing below the application layer will ever
//! complain. That is the only case these deadlines exist for.
//!
//! It matters here because the job heartbeat renews the lease while a worker
//! waits, so a stall no one can see would otherwise be indefinite rather than
//! bounded by the lease.

use std::time::Duration;

/// How long to wait for a connection to be established.
///
/// Short: a host that cannot be reached at all should fail over quickly
/// rather than making someone wait on a machine that is down.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a stream may go without delivering anything.
///
/// An idle timeout, not a total one: reqwest resets it after each successful
/// read, so a generation that takes five minutes while producing tokens is
/// never interrupted. Only complete silence trips it.
///
/// Five minutes is deliberately far beyond any healthy pause. The cost of
/// being wrong is asymmetric -- too short aborts turns that were merely slow
/// and sends every one of them back through the job queue as a retry, where
/// too long only delays noticing something that was never going to answer.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// A client for calls that stream a response over a long period.
///
/// The idle timeout is a parameter rather than a constant so a test can prove
/// the deadline works without waiting five minutes for it.
pub fn streaming_client(idle: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(idle)
        .build()
        // Only fails if the TLS backend cannot be initialised, which is a
        // deployment fault rather than a runtime condition.
        .expect("failed to build HTTP client")
}

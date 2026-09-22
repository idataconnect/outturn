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

/// A client for sending a long stream and waiting for one answer at the end.
///
/// Deliberately without a read timeout, which is the difference between this
/// and `streaming_client` and the reason both exist. The two describe opposite
/// shapes:
///
/// - Reading a streamed response, where silence means the peer has stalled and
///   a deadline is the only thing that will ever notice.
/// - Writing a streamed body to a peer that answers once, at the end. Here
///   silence is the normal condition for as long as the work takes, and a read
///   deadline measures the length of the turn rather than the health of the
///   connection.
///
/// The runtime reports a turn's events as a request body and the API replies
/// with a status once it has consumed them all. Sharing `streaming_client` for
/// that meant a turn lasting longer than `IDLE_TIMEOUT` aborted its own
/// reporting: the turn itself carried on and completed, but the browser saw
/// nothing more of it, and a later reconnect resumed from the live edge -- so
/// a reader watched a reply stop mid-sentence, and found the missing middle
/// only after a refresh. A block of tool calls is the ordinary way to spend
/// five minutes.
///
/// A peer that dies is still caught: TCP keepalive errors a dead connection
/// within about a minute, and the job lease bounds the rest.
pub fn reporting_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        // Only fails if the TLS backend cannot be initialised, which is a
        // deployment fault rather than a runtime condition.
        .expect("failed to build HTTP client")
}

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

#[cfg(test)]
mod tests {
    use super::*;

    /// A long turn must not abort its own reporting.
    ///
    /// The runtime streams a turn's events as a request body and the API
    /// answers once, at the end -- so the connection is silent for as long as
    /// the turn takes. Reporting through `streaming_client` made that silence
    /// a fault: a turn past `IDLE_TIMEOUT` killed the channel carrying its own
    /// output while the turn itself ran on, and a reader watched a reply stop
    /// mid-sentence with the rest of it sitting in the database.
    ///
    /// Driven against a real socket rather than by inspecting the builder,
    /// because what is being asserted is behaviour reqwest provides and a
    /// field this crate cannot read back.
    #[tokio::test]
    async fn reporting_waits_out_a_silence_that_streaming_would_not() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");

        // Accepts, reads nothing, and answers after a pause -- which is what
        // the API does while a turn is still being consumed.
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    use tokio::io::AsyncWriteExt;
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    let _ = socket
                        .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                        .await;
                });
            }
        });

        let url = format!("http://{addr}/");

        // A read deadline shorter than the peer's silence: this is the failure
        // being fixed, reproduced.
        let streaming = streaming_client(Duration::from_millis(50));
        assert!(
            streaming.post(&url).send().await.is_err(),
            "a read timeout shorter than the work must abort, or this test \
             proves nothing about the client that does not have one"
        );

        // The same silence, waited out.
        let reporting = reporting_client();
        let response = reporting.post(&url).send().await;
        assert!(
            response.is_ok(),
            "reporting must survive a peer that answers only at the end: {:?}",
            response.err()
        );
    }
}

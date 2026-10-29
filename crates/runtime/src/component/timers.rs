//! Waiting: a sleep that ends the turn, and timers that start a later one.
//! Both are relayed to the API, which owns the schedule.

use super::{AgentHost, TimerDue, WaitRequest};

impl AgentHost {
    pub(super) async fn sleep(&mut self, seconds: u64, reason: String) -> Result<String, String> {
        let Some(relay) = &self.on_wait else {
            return Err("sleeping is not available here".into());
        };
        let answer = relay(WaitRequest::Sleep { seconds, reason }).await?;
        // The turn ends at its next round boundary, on the flag a stop uses --
        // which is the only place stopping is safe. It is not a cancellation:
        // the job completes normally, the reply written so far is kept, and
        // the conversation is held until the wakeup, which the API arranged.
        self.cancelled = true;
        Ok(answer)
    }

    pub(super) async fn set_timer(
        &mut self,
        due: TimerDue,
        reason: String,
    ) -> Result<String, String> {
        let Some(relay) = &self.on_wait else {
            return Err("timers are not available here".into());
        };
        let (at, seconds) = match due {
            TimerDue::At(at) => (Some(at), None),
            TimerDue::Seconds(s) => (None, Some(s)),
        };
        relay(WaitRequest::Timer {
            at,
            seconds,
            reason,
        })
        .await
    }

    pub(super) async fn list_timers(&mut self) -> Result<String, String> {
        let Some(relay) = &self.on_wait else {
            return Err("timers are not available here".into());
        };
        relay(WaitRequest::ListTimers).await
    }

    pub(super) async fn cancel_timer(&mut self, id: String) -> Result<String, String> {
        let Some(relay) = &self.on_wait else {
            return Err("timers are not available here".into());
        };
        relay(WaitRequest::CancelTimer { id }).await
    }
}

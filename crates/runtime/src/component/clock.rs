//! The time, in the zone of the user the turn belongs to.

use super::{AgentHost, Clock};
use outturn_core::time::Timestamp;

impl AgentHost {
    pub(super) async fn current_time(&mut self) -> Clock {
        // The zone belongs to the user, not to this machine: the runtime runs
        // in a container that is almost certainly UTC, so answering from its
        // own locale would be confidently wrong for everyone.
        use outturn_core::time::SecondsFormat;

        match &self.timezone {
            Some(tz) => {
                let now = Timestamp::now().as_jiff().to_zoned_datetime(tz.clone());
                Clock {
                    now: now.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string(),
                    weekday: now.strftime("%A").to_string(),
                    timezone: tz.iana_name().unwrap_or("UTC").to_string(),
                    abbreviation: now.strftime("%Z").to_string(),
                }
            }
            None => {
                let now = Timestamp::now();
                Clock {
                    now: now.to_rfc3339_opts(SecondsFormat::Secs, true),
                    weekday: now.as_jiff().strftime("%A").to_string(),
                    timezone: String::new(),
                    abbreviation: "UTC".to_string(),
                }
            }
        }
    }
}

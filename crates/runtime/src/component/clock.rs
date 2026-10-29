//! The time, in the zone of the user the turn belongs to.

use super::{AgentHost, Clock};

impl AgentHost {
    pub(super) async fn current_time(&mut self) -> Clock {
        // The zone belongs to the user, not to this machine: the runtime runs
        // in a container that is almost certainly UTC, so answering from its
        // own locale would be confidently wrong for everyone.
        use chrono::SecondsFormat;

        match self.timezone {
            Some(tz) => {
                let now = chrono::Utc::now().with_timezone(&tz);
                Clock {
                    now: now.to_rfc3339_opts(SecondsFormat::Secs, false),
                    weekday: now.format("%A").to_string(),
                    timezone: tz.name().to_string(),
                    abbreviation: now.format("%Z").to_string(),
                }
            }
            None => {
                let now = chrono::Utc::now();
                Clock {
                    now: now.to_rfc3339_opts(SecondsFormat::Secs, true),
                    weekday: now.format("%A").to_string(),
                    timezone: String::new(),
                    abbreviation: "UTC".to_string(),
                }
            }
        }
    }
}

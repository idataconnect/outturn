//! Holds, as the API shows them: the shared rules from `crate::inhibitor`,
//! and who holds each one, which only this tier can say.

use std::collections::HashMap;

use serde::Serialize;
use uuid::Uuid;

use crate::api::actor::Actor;
pub use crate::inhibitor::*;

/// A hold as a person reading it is shown it: with who holds it, in words.
#[derive(Debug, Clone, Serialize)]
pub struct Held {
    #[serde(flatten)]
    pub inhibitor: Inhibitor,
    pub holder: Actor,
}

/// Who holds a hold, from what took it.
///
/// `held_by` is a user's id or a machine's name, so it is read here rather than
/// shown: a user by the rules every actor is named by (`api::actor`), the two
/// holds the platform takes for itself in words, and any other machine name as
/// it was given -- a spend cap's credential is named by whoever set it up.
pub fn holder(held_by: &str, users: &HashMap<Uuid, Actor>) -> Actor {
    if let Ok(id) = held_by.parse::<Uuid>() {
        return users.get(&id).cloned().unwrap_or_default();
    }
    let name = match held_by {
        crate::api::gated::GATE_HOLDER => "a rule that needs approval",
        crate::api::wake::SLEEP_HOLDER => "the agent, asleep",
        other => other,
    };
    Actor::System {
        name: name.to_string(),
    }
}

/// These holds with their holders, looking every person up in one query.
pub async fn with_holders(
    pool: &sqlx::PgPool,
    held: Vec<Inhibitor>,
) -> Result<Vec<Held>, InhibitorError> {
    let ids: Vec<Uuid> = held
        .iter()
        .filter_map(|i| i.held_by.parse::<Uuid>().ok())
        .collect();
    let users = crate::api::actor::users(pool, &ids)
        .await
        .map_err(|e| InhibitorError::Internal(e.to_string()))?;
    Ok(held
        .into_iter()
        .map(|inhibitor| Held {
            holder: holder(&inhibitor.held_by, &users),
            inhibitor,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_holder_is_named_in_words_never_as_an_id() {
        let ana = Uuid::now_v7();
        let staff = Uuid::now_v7();
        let users = HashMap::from([
            (ana, Actor::Person { name: "Ana".into() }),
            (staff, Actor::Operator),
        ]);
        assert_eq!(
            holder(&ana.to_string(), &users),
            Actor::Person { name: "Ana".into() }
        );
        assert_eq!(holder(&staff.to_string(), &users), Actor::Operator);
        // An account that is gone says nothing rather than showing its id.
        assert_eq!(
            holder(&Uuid::now_v7().to_string(), &users),
            Actor::Unrecorded
        );
        assert_eq!(
            holder(crate::api::gated::GATE_HOLDER, &users),
            Actor::System {
                name: "a rule that needs approval".into()
            }
        );
        assert_eq!(
            holder(crate::api::wake::SLEEP_HOLDER, &users),
            Actor::System {
                name: "the agent, asleep".into()
            }
        );
        assert_eq!(
            holder("billing-bot", &users),
            Actor::System {
                name: "billing-bot".into()
            }
        );
    }
}

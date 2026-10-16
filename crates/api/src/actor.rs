//! Who did something, as the person reading it may be told.
//!
//! Every record that keeps who acted -- who wrote a skill version, who stopped
//! a workspace -- is read by somebody who may or may not have a relationship
//! with that actor. What they are told is decided here, once:
//!
//! - The operator's staff are the operator. A platform admin is named to no
//!   customer, even in a workspace they also belong to: the capacity they
//!   acted in is not recorded, and naming them is the guess that cannot be
//!   taken back.
//! - Anybody else is named by their display name.
//! - A rule or a machine that acted is said to have, in words rather than an
//!   id.
//! - An actor that cannot be found -- nobody signed, or the account is gone --
//!   is said nothing about rather than guessed at.

use std::collections::HashMap;

use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Actor {
    Person {
        name: String,
    },
    Operator,
    /// Something other than a person: a rule, the agent itself, a machine
    /// credential. `name` is already the words to show.
    System {
        name: String,
    },
    #[default]
    Unrecorded,
}

impl Actor {
    /// A user, from their display name and whether they are the operator's
    /// staff. A name of `None` is an account that is not there.
    pub fn user(name: Option<String>, operator: bool) -> Self {
        match (name, operator) {
            (Some(_), true) => Actor::Operator,
            (Some(name), false) => Actor::Person { name },
            (None, _) => Actor::Unrecorded,
        }
    }
}

/// Whether the user in `$column` is the operator's staff, as SQL, for a query
/// that reads its actors in the same statement rather than through `users`.
/// The one place the rule for staff is written in SQL.
#[macro_export]
macro_rules! operator_staff_sql {
    ($column:literal) => {
        concat!(
            "exists (select 1 from user_system_roles r where r.user_id = ",
            $column,
            " and r.role = 'system_admin')"
        )
    };
}

/// The actors behind some user ids, in one query.
///
/// An id with no account behind it is absent from the map, which a caller
/// reads as `Actor::Unrecorded`.
pub async fn users(pool: &sqlx::PgPool, ids: &[Uuid]) -> Result<HashMap<Uuid, Actor>, sqlx::Error> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(Uuid, String, bool)> = sqlx::query_as(concat!(
        "select u.id, u.display_name, ",
        operator_staff_sql!("u.id"),
        " from users u where u.id = any($1)"
    ))
    .bind(ids)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, name, operator)| (id, Actor::user(Some(name), operator)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::Actor;

    #[test]
    fn the_operators_staff_are_never_named() {
        assert_eq!(
            Actor::user(Some("Ana".into()), false),
            Actor::Person { name: "Ana".into() }
        );
        assert_eq!(Actor::user(Some("Staff".into()), true), Actor::Operator);
        assert_eq!(Actor::user(None, true), Actor::Unrecorded);
        assert_eq!(Actor::user(None, false), Actor::Unrecorded);
    }
}

use async_trait::async_trait;
use sqlx::Row;
use sqlx::postgres::PgPool;
use uuid::Uuid;

use super::{
    ActionError, ActionItem, ActionStore, Delivery, NewItem, State, Target, dedupe_targets,
    validate,
};

/// Postgres channel an action queue change is announced on.
///
/// Separate from `outturn_events` because the audiences differ: an event hint
/// wakes whoever is reading a conversation, and this wakes whoever is showing
/// a queue. Sharing one channel would wake every queue on every streamed
/// token.
pub const CHANNEL: &str = "outturn_actions";

pub struct PostgresActionStore {
    pool: PgPool,
}

fn internal(e: sqlx::Error) -> ActionError {
    ActionError::Internal(e.to_string())
}

impl PostgresActionStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn read_item(row: &sqlx::postgres::PgRow) -> Result<ActionItem, ActionError> {
    let state: String = row.get("state");
    let state = State::parse(&state).ok_or_else(|| {
        // The column is constrained, so this is drift between the constraint
        // and the enum rather than bad data -- worth saying plainly.
        ActionError::Internal(format!("unknown action item state {state:?}"))
    })?;
    Ok(ActionItem {
        id: row.get("id"),
        workspace_id: row.get("workspace_id"),
        kind: row.get("kind"),
        event_id: row.try_get("event_id").ok().flatten(),
        payload: row.get("payload"),
        state,
        resolved_note: row.try_get("resolved_note").ok().flatten(),
        created_at: row.get("created_at"),
        expires_at: row.try_get("expires_at").ok().flatten(),
    })
}

/// Writes the target rows for an item.
///
/// `on conflict do nothing` against the two partial unique indexes, so adding
/// a target that is already there is not an error -- escalation re-adding a
/// role it already named should not have to know whether it did.
async fn insert_targets(
    conn: &mut sqlx::PgConnection,
    workspace_id: Uuid,
    item_id: Uuid,
    targets: &[Target],
) -> Result<(), ActionError> {
    for target in targets {
        sqlx::query(
            "insert into action_targets (workspace_id, item_id, role_id, user_id) \
             values ($1, $2, $3, $4) on conflict do nothing",
        )
        .bind(workspace_id)
        .bind(item_id)
        .bind(target.role_id())
        .bind(target.user_id())
        .execute(&mut *conn)
        .await
        .map_err(internal)?;
    }
    Ok(())
}

/// Announces a queue change on `CHANNEL`.
///
/// The payload addresses only -- workspace, and which targets changed. Clients
/// refetch from it, the same bargain `events::notify` makes: a coalesced or
/// dropped notification costs a refetch rather than a wrong queue, and the
/// payload stays well under the 8000-byte NOTIFY limit however many items
/// moved.
async fn announce(
    conn: &mut sqlx::PgConnection,
    workspace_id: Uuid,
    delivery: &Delivery,
) -> Result<(), ActionError> {
    let roles: Vec<Uuid> = delivery
        .targets()
        .iter()
        .filter_map(Target::role_id)
        .collect();
    let users: Vec<Uuid> = delivery
        .targets()
        .iter()
        .filter_map(Target::user_id)
        .collect();

    let payload = serde_json::json!({
        "workspace_id": workspace_id,
        "roles": roles,
        "users": users,
        // Clients do not act on this, but an invalidation in the log is worth
        // being able to find: it is the path that is supposed to be rare.
        "invalidate": matches!(delivery, Delivery::Invalidate { .. }),
    });

    sqlx::query("select pg_notify($1, $2)")
        .bind(CHANNEL)
        .bind(payload.to_string())
        .execute(&mut *conn)
        .await
        .map_err(internal)?;
    Ok(())
}

#[async_trait]
impl ActionStore for PostgresActionStore {
    async fn raise(&self, workspace_id: Uuid, item: NewItem) -> Result<Uuid, ActionError> {
        validate(&item)?;
        let targets = dedupe_targets(&item.targets);

        let mut tx = self.pool.begin().await.map_err(internal)?;
        let id = Uuid::now_v7();

        sqlx::query(
            "insert into action_items \
             (workspace_id, id, kind, event_id, payload, expires_at) \
             values ($1, $2, $3, $4, $5, $6)",
        )
        .bind(workspace_id)
        .bind(id)
        .bind(&item.kind)
        .bind(item.event_id)
        .bind(&item.payload)
        .bind(item.expires_at)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;

        insert_targets(&mut tx, workspace_id, id, &targets).await?;

        // Announced inside the transaction: pg_notify is transactional, so a
        // rollback takes the announcement with it and nobody is woken for an
        // item that does not exist.
        announce(
            &mut tx,
            workspace_id,
            &Delivery::Targeted {
                item_ids: vec![id],
                targets,
            },
        )
        .await?;

        tx.commit().await.map_err(internal)?;
        Ok(id)
    }

    async fn settle(
        &self,
        workspace_id: Uuid,
        item_id: Uuid,
        state: State,
        resolved_by: Option<Uuid>,
    ) -> Result<(), ActionError> {
        if state.is_open() {
            return Err(ActionError::Invalid(
                "settling an item requires a state that is not pending".into(),
            ));
        }

        let mut tx = self.pool.begin().await.map_err(internal)?;

        // `where state = 'pending'` in the update rather than a read first:
        // two people answering at once both pass a check-then-write, and only
        // one passes this. The loser learns it lost instead of overwriting the
        // winner's answer.
        let updated = sqlx::query(
            "update action_items set state = $3, resolved_by = $4, resolved_at = now() \
             where workspace_id = $1 and id = $2 and state = 'pending'",
        )
        .bind(workspace_id)
        .bind(item_id)
        .bind(state.as_str())
        .bind(resolved_by)
        .execute(&mut *tx)
        .await
        .map_err(internal)?
        .rows_affected();

        if updated == 0 {
            // Tell the two cases apart: an item that was never there and an
            // item somebody else already settled are different problems.
            let existing: Option<String> = sqlx::query_scalar(
                "select state from action_items where workspace_id = $1 and id = $2",
            )
            .bind(workspace_id)
            .bind(item_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(internal)?;

            return Err(match existing.as_deref().and_then(State::parse) {
                Some(s) => ActionError::NotPending(s.as_str()),
                None => ActionError::NotFound,
            });
        }

        // Read the targets back rather than making the caller supply them:
        // whoever settles an item knows the item, not who was waiting on it,
        // and a caller guessing would be a caller inventing an invalidation.
        let targets = targets_of(&mut tx, workspace_id, item_id).await?;

        announce(
            &mut tx,
            workspace_id,
            &Delivery::Targeted {
                item_ids: vec![item_id],
                targets,
            },
        )
        .await?;

        tx.commit().await.map_err(internal)?;
        Ok(())
    }

    async fn add_targets(
        &self,
        workspace_id: Uuid,
        item_id: Uuid,
        targets: &[Target],
    ) -> Result<(), ActionError> {
        if targets.is_empty() {
            return Ok(());
        }
        let targets = dedupe_targets(targets);

        let mut tx = self.pool.begin().await.map_err(internal)?;

        // Existence checked explicitly: `insert ... on conflict do nothing`
        // against a missing item fails on the foreign key, which is a
        // constraint violation rather than the NotFound the caller can act on.
        let exists: Option<Uuid> =
            sqlx::query_scalar("select id from action_items where workspace_id = $1 and id = $2")
                .bind(workspace_id)
                .bind(item_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(internal)?;
        if exists.is_none() {
            return Err(ActionError::NotFound);
        }

        insert_targets(&mut tx, workspace_id, item_id, &targets).await?;
        announce(
            &mut tx,
            workspace_id,
            &Delivery::Targeted {
                item_ids: vec![item_id],
                targets,
            },
        )
        .await?;

        tx.commit().await.map_err(internal)?;
        Ok(())
    }

    async fn remove_targets(
        &self,
        workspace_id: Uuid,
        item_id: Uuid,
        targets: &[Target],
    ) -> Result<(), ActionError> {
        if targets.is_empty() {
            return Ok(());
        }
        let targets = dedupe_targets(targets);

        let mut tx = self.pool.begin().await.map_err(internal)?;
        for target in &targets {
            sqlx::query(
                "delete from action_targets \
                 where workspace_id = $1 and item_id = $2 \
                   and role_id is not distinct from $3 \
                   and user_id is not distinct from $4",
            )
            .bind(workspace_id)
            .bind(item_id)
            .bind(target.role_id())
            .bind(target.user_id())
            .execute(&mut *tx)
            .await
            .map_err(internal)?;
        }

        // Announced to the targets that lost the item, not the ones that
        // still have it: their queue is what changed.
        announce(
            &mut tx,
            workspace_id,
            &Delivery::Targeted {
                item_ids: vec![item_id],
                targets,
            },
        )
        .await?;

        tx.commit().await.map_err(internal)?;
        Ok(())
    }

    async fn queue_for_user(
        &self,
        workspace_id: Uuid,
        user_id: Uuid,
    ) -> Result<Vec<ActionItem>, ActionError> {
        // Role membership joined at read time rather than stored per user.
        // This is the one place the derived form is kept, and deliberately:
        // it is what lets somebody who joins a role today see the item raised
        // yesterday, with no queue row written when membership changes.
        let rows = sqlx::query(
            "select distinct i.workspace_id, i.id, i.kind, i.event_id, i.payload, \
                    i.state, i.resolved_note, i.created_at, i.expires_at \
             from action_items i \
             join action_targets t \
               on t.workspace_id = i.workspace_id and t.item_id = i.id \
             where i.workspace_id = $1 and i.state = 'pending' \
               and (t.user_id = $2 or t.role_id in ( \
                     select role_id from user_workspace_roles \
                     where user_id = $2 and workspace_id = $1)) \
             order by i.id",
        )
        .bind(workspace_id)
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        rows.iter().map(read_item).collect()
    }

    async fn queue_for_role(
        &self,
        workspace_id: Uuid,
        role_id: Uuid,
    ) -> Result<Vec<ActionItem>, ActionError> {
        let rows = sqlx::query(
            "select i.workspace_id, i.id, i.kind, i.event_id, i.payload, \
                    i.state, i.resolved_note, i.created_at, i.expires_at \
             from action_items i \
             join action_targets t \
               on t.workspace_id = i.workspace_id and t.item_id = i.id \
             where i.workspace_id = $1 and t.role_id = $2 and i.state = 'pending' \
             order by i.id",
        )
        .bind(workspace_id)
        .bind(role_id)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        rows.iter().map(read_item).collect()
    }

    async fn count_for_user(&self, workspace_id: Uuid, user_id: Uuid) -> Result<i64, ActionError> {
        // `count(distinct i.id)` rather than `count(*)`: an item targeted at
        // somebody directly *and* at a role they hold joins twice, and a badge
        // reading two for one decision is a badge nobody trusts.
        let count: i64 = sqlx::query_scalar(
            "select count(distinct i.id) from action_items i \
             join action_targets t \
               on t.workspace_id = i.workspace_id and t.item_id = i.id \
             where i.workspace_id = $1 and i.state = 'pending' \
               and (t.user_id = $2 or t.role_id in ( \
                     select role_id from user_workspace_roles \
                     where user_id = $2 and workspace_id = $1))",
        )
        .bind(workspace_id)
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(internal)?;
        Ok(count)
    }

    async fn queue_for_user_everywhere(
        &self,
        user_id: Uuid,
        limit: i64,
    ) -> Result<Vec<ActionItem>, ActionError> {
        // No `workspace_id` predicate anywhere, by design -- and so the join
        // to `user_workspace_roles` is the only thing admitting a workspace at
        // all. Note it is joined twice for two different jobs, and dropping
        // either one leaks:
        //
        //   `m` decides which workspaces the reader belongs to. Without it a
        //   direct user target would be returned from a workspace they left.
        //
        //   `r` decides which role targets within those workspaces reach them.
        //   Without it every item in a workspace they belong to would arrive,
        //   whoever it was addressed to.
        // Two `exists` rather than joins plus `distinct`. An item is wanted once
        // however many of its targets match the reader, and deduplicating by
        // sorting every selected column forced a sort of the whole candidate
        // set before the limit could apply -- measured at 346k items, that was
        // a sequential scan of the targets table and a full sort to return
        // fifty rows. As `exists`, the limit stops the work early.
        let rows = sqlx::query(
            "select i.workspace_id, i.id, i.kind, i.event_id, i.payload, \
                    i.state, i.resolved_note, i.created_at, i.expires_at \
             from action_items i \
             where i.state = 'pending' \
               and exists (select 1 from user_workspace_roles m \
                           where m.workspace_id = i.workspace_id and m.user_id = $1) \
               and exists ( \
                   select 1 from action_targets t \
                   where t.workspace_id = i.workspace_id and t.item_id = i.id \
                     and (t.user_id = $1 or exists ( \
                           select 1 from user_workspace_roles r \
                           where r.user_id = $1 \
                             and r.workspace_id = i.workspace_id \
                             and r.role_id = t.role_id))) \
             order by i.id limit $2",
        )
        .bind(user_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        rows.iter().map(read_item).collect()
    }

    async fn count_for_user_everywhere(&self, user_id: Uuid, cap: i64) -> Result<i64, ActionError> {
        // Counted over a capped subquery rather than as a bare `count(*)`: the
        // number is rendered as "and more" past the cap anyway, and an
        // unbounded count over every workspace a person belongs to is a scan
        // whose cost is set by the busiest of them.
        // The same `exists` shape as the listing, and for a sharper reason: as
        // a join with `distinct`, the cap applied only after the join had been
        // built, so it bounded the answer without bounding the work. At 346k
        // items that was 52ms of sequential scan for a number rendered as
        // "and more"; as `exists` the cap stops the scan, and it was 6.8ms.
        let count: i64 = sqlx::query_scalar(
            "select count(*) from ( \
                 select i.id from action_items i \
                 where i.state = 'pending' \
                   and exists (select 1 from user_workspace_roles m \
                               where m.workspace_id = i.workspace_id and m.user_id = $1) \
                   and exists ( \
                       select 1 from action_targets t \
                       where t.workspace_id = i.workspace_id and t.item_id = i.id \
                         and (t.user_id = $1 or exists ( \
                               select 1 from user_workspace_roles r \
                               where r.user_id = $1 \
                                 and r.workspace_id = i.workspace_id \
                                 and r.role_id = t.role_id))) \
                 limit $2) capped",
        )
        .bind(user_id)
        .bind(cap)
        .fetch_one(&self.pool)
        .await
        .map_err(internal)?;
        Ok(count)
    }

    async fn settle_and_release(
        &self,
        workspace_id: Uuid,
        item_id: Uuid,
        state: State,
        resolved_by: Option<Uuid>,
        note: Option<&str>,
        hold: Option<Uuid>,
    ) -> Result<u64, ActionError> {
        if state.is_open() {
            return Err(ActionError::Invalid(
                "settling an item requires a state that is not pending".into(),
            ));
        }

        let mut tx = self.pool.begin().await.map_err(internal)?;

        // The same conditional update `settle` uses, and for the same reason:
        // two people answering at once produce one winner and one NotPending
        // rather than a silently overwritten decision. Inside the transaction
        // now, so the loser's release never happens either.
        let updated = sqlx::query(
            "update action_items set state = $3, resolved_by = $4, \
                    resolved_note = $5, resolved_at = now() \
             where workspace_id = $1 and id = $2 and state = 'pending'",
        )
        .bind(workspace_id)
        .bind(item_id)
        .bind(state.as_str())
        .bind(resolved_by)
        .bind(note)
        .execute(&mut *tx)
        .await
        .map_err(internal)?
        .rows_affected();

        if updated == 0 {
            let existing: Option<String> = sqlx::query_scalar(
                "select state from action_items where workspace_id = $1 and id = $2",
            )
            .bind(workspace_id)
            .bind(item_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(internal)?;

            return Err(match existing.as_deref().and_then(State::parse) {
                Some(s) => ActionError::NotPending(s.as_str()),
                None => ActionError::NotFound,
            });
        }

        let mut resumed = 0;
        if let Some(hold) = hold {
            // Read for its scope before it goes, because the scope is what says
            // how wide the resume should be. The hold is the authority on that,
            // not a copy of the session id somewhere else.
            let scope: Option<(String, Option<Uuid>, Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
                "select level, workspace_id, agent_id, session_id from inhibitors \
                     where id = $1",
            )
            .bind(hold)
            .fetch_optional(&mut *tx)
            .await
            .map_err(internal)?;

            // A hold that is already gone is not an error. Somebody released it
            // by hand between the queue read and here, and what this call is for
            // -- the work no longer being held -- is already true.
            if let Some((level, held_workspace, agent_id, session_id)) = scope {
                sqlx::query("delete from inhibitors where id = $1")
                    .bind(hold)
                    .execute(&mut *tx)
                    .await
                    .map_err(internal)?;

                // Scoped as the hold was. A platform hold resumes nothing: it is
                // not a workspace's to lift, and nothing addresses one here.
                let held_workspace = held_workspace.unwrap_or(workspace_id);
                let (agent, session) = match level.as_str() {
                    "agent" => (agent_id, None),
                    "session" => (None, session_id),
                    "workspace" => (None, None),
                    _ => (None, None),
                };
                if level != "platform" {
                    resumed = sqlx::query(
                        "update jobs set \
                             state = 'pending', \
                             run_after = now(), \
                             last_error = null, \
                             updated_at = now() \
                         where state = 'parked' \
                           and workspace_id = $1 \
                           and ($2::uuid is null or (payload->>'agent_id')::uuid = $2) \
                           and ($3::uuid is null or (payload->>'session_id')::uuid = $3)",
                    )
                    .bind(held_workspace)
                    .bind(agent)
                    .bind(session)
                    .execute(&mut *tx)
                    .await
                    .map_err(internal)?
                    .rows_affected();
                }
            }
        }

        let targets = targets_of(&mut tx, workspace_id, item_id).await?;
        announce(
            &mut tx,
            workspace_id,
            &Delivery::Targeted {
                item_ids: vec![item_id],
                targets,
            },
        )
        .await?;

        tx.commit().await.map_err(internal)?;
        Ok(resumed)
    }

    async fn settled_for_user(
        &self,
        user_id: Uuid,
        item_id: Uuid,
    ) -> Result<Option<State>, ActionError> {
        // The same two `exists` clauses as the global read, minus the state
        // filter: one admits the workspace through membership, the other
        // matches the target. Dropping either would answer for somebody else's
        // item, which is the whole reason this is not a read by id.
        let state: Option<String> = sqlx::query_scalar(
            "select i.state from action_items i \
             where i.id = $2 \
               and i.state <> 'pending' \
               and exists (select 1 from user_workspace_roles m \
                           where m.workspace_id = i.workspace_id and m.user_id = $1) \
               and exists ( \
                   select 1 from action_targets t \
                   where t.workspace_id = i.workspace_id and t.item_id = i.id \
                     and (t.user_id = $1 or exists ( \
                           select 1 from user_workspace_roles r \
                           where r.user_id = $1 \
                             and r.workspace_id = i.workspace_id \
                             and r.role_id = t.role_id)))",
        )
        .bind(user_id)
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;

        match state {
            Some(s) => State::parse(&s)
                .map(Some)
                .ok_or_else(|| ActionError::Internal(format!("unknown action item state {s:?}"))),
            None => Ok(None),
        }
    }

    async fn roles_of(&self, user_id: Uuid) -> Result<Vec<Uuid>, ActionError> {
        let roles: Vec<Uuid> =
            sqlx::query_scalar("select role_id from user_workspace_roles where user_id = $1")
                .bind(user_id)
                .fetch_all(&self.pool)
                .await
                .map_err(internal)?;
        Ok(roles)
    }

    async fn deliver(&self, workspace_id: Uuid, delivery: Delivery) -> Result<(), ActionError> {
        // A delivery that names nothing wakes every client showing a queue to
        // refetch an unchanged answer. Cheap to check, and the check is the
        // difference between an idle cluster and a stampede on a no-op.
        if delivery.is_empty() {
            return Ok(());
        }
        let mut conn = self.pool.acquire().await.map_err(internal)?;
        announce(&mut conn, workspace_id, &delivery).await
    }

    async fn sweep_expired(&self, limit: i64) -> Result<u64, ActionError> {
        // Bounded by `limit` so a backlog is worked through over several ticks
        // rather than in one statement that locks a large span of the table.
        let swept = sqlx::query(
            "update action_items set state = 'expired', resolved_at = now() \
             where (workspace_id, id) in ( \
                 select workspace_id, id from action_items \
                 where state = 'pending' and expires_at is not null and expires_at < now() \
                 order by expires_at limit $1)",
        )
        .bind(limit)
        .execute(&self.pool)
        .await
        .map_err(internal)?
        .rows_affected();

        // No announcement here. The sweep does not know which workspaces it
        // touched without reading them back, and an expiry is not urgent --
        // the next real change refetches, and a badge briefly one too high is
        // better than a per-row query on a background tick. Worth revisiting
        // if expiry ever becomes the common way items leave the queue.
        Ok(swept)
    }

    async fn orphaned(
        &self,
        workspace_id: Uuid,
        limit: i64,
    ) -> Result<Vec<ActionItem>, ActionError> {
        let rows = sqlx::query(
            "select i.workspace_id, i.id, i.kind, i.event_id, i.payload, \
                    i.state, i.resolved_note, i.created_at, i.expires_at \
             from action_items i \
             where i.workspace_id = $1 and i.state = 'pending' \
               and not exists (select 1 from action_targets t \
                               where t.workspace_id = i.workspace_id and t.item_id = i.id) \
             order by i.id limit $2",
        )
        .bind(workspace_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        rows.iter().map(read_item).collect()
    }
}

/// Who is currently waiting on an item.
async fn targets_of(
    conn: &mut sqlx::PgConnection,
    workspace_id: Uuid,
    item_id: Uuid,
) -> Result<Vec<Target>, ActionError> {
    let rows = sqlx::query(
        "select role_id, user_id from action_targets \
         where workspace_id = $1 and item_id = $2",
    )
    .bind(workspace_id)
    .bind(item_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(internal)?;

    Ok(rows
        .iter()
        .filter_map(|r| {
            let role: Option<Uuid> = r.get("role_id");
            let user: Option<Uuid> = r.get("user_id");
            match (role, user) {
                (Some(id), None) => Some(Target::Role(id)),
                (None, Some(id)) => Some(Target::User(id)),
                // The check constraint refuses both and neither, so this is
                // unreachable unless the constraint was dropped.
                _ => None,
            }
        })
        .collect())
}

//! The skill figures, computed where the rows are.

use async_trait::async_trait;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::api::actor::Actor;

use super::stats::{
    IdleSkill, LaggingSkill, SkillAuthor, SkillStats, SkillStatsStore, SkillTotals, SkillUse,
    StatsError,
};

pub struct PostgresSkillStatsStore {
    pool: PgPool,
}

impl PostgresSkillStatsStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn internal(e: sqlx::Error) -> StatsError {
    StatsError::Internal(e.to_string())
}

// Every query here joins `turn_skills` through `agent_messages` for its time:
// the join row names the reply, and the reply is what carries the clock. Ids
// are UUIDv7 and would order correctly, but Postgres has no `max(uuid)` and a
// hand-rolled decode would be a second way of reading a time.
//
// Written out at each site rather than shared through a `format!`, because
// building SQL from strings is linted against here -- rightly, and a constant
// with no user input in it is not worth the exception.

#[async_trait]
impl SkillStatsStore for PostgresSkillStatsStore {
    async fn stats(
        &self,
        workspace_id: Uuid,
        from: chrono::DateTime<chrono::Utc>,
        to: chrono::DateTime<chrono::Utc>,
        limit: i64,
    ) -> Result<SkillStats, StatsError> {
        // Every window over turns is bounded twice: by the reply's id, which
        // is a UUIDv7 and so a range of `turn_skills`' key (`uuid7_floor`),
        // and by `created_at`, which decides. The id range is what keeps a
        // window from reading every turn ever recorded; the minute of slack
        // either side covers the app minting the id a moment before the
        // database stamps the row.
        //
        // The standing shape of the workspace's skills, and what happened to
        // them in the window. One statement because these are all counts over
        // the same two tables, and six round trips to fill one panel is six
        // chances for the figures to disagree with each other.
        let totals = sqlx::query(
            "select \
                 (select count(*) from skills \
                   where workspace_id = $1 and retired_at is null) as skills, \
                 (select count(distinct a.skill_id) from agent_skills a \
                    join skills s on s.id = a.skill_id \
                   where a.workspace_id = $1 and s.retired_at is null) as bound, \
                 (select count(*) from skill_versions \
                   where workspace_id = $1 and created_at >= $2 and created_at < $3) as versions, \
                 (select count(*) from skills \
                   where workspace_id = $1 and created_at >= $2 and created_at < $3) as created, \
                 (select count(*) from skills \
                   where workspace_id = $1 \
                     and retired_at >= $2 and retired_at < $3) as retired, \
                 (select count(*) from turn_skills ts \
                    join agent_messages m on m.id = ts.reply_id \
                    join skills s on s.id = ts.skill_id \
                   where s.workspace_id = $1 \
                     and ts.reply_id >= uuid7_floor($2 - interval '1 minute') and ts.reply_id < uuid7_floor($3 + interval '1 minute') \
                     and m.created_at >= $2 and m.created_at < $3) as turns",
        )
        .bind(workspace_id)
        .bind(from)
        .bind(to)
        .fetch_one(&self.pool)
        .await
        .map_err(internal)?;

        let totals = SkillTotals {
            skills: totals.get("skills"),
            bound: totals.get("bound"),
            versions: totals.get("versions"),
            created: totals.get("created"),
            retired: totals.get("retired"),
            turns: totals.get("turns"),
        };

        // What each skill served. Sessions as well as turns, because one
        // conversation calling a skill twenty times and twenty conversations
        // calling it once are different facts about how much it is relied on.
        let used = sqlx::query(
            "select s.id, s.name, s.slug, \
                    count(*) as turns, \
                    count(distinct m.session_id) as sessions, \
                    max(m.created_at) as last_used, \
                    (select count(*) from skill_versions v \
                      where v.skill_id = s.id \
                        and v.created_at >= $2 and v.created_at < $3) as versions \
               from turn_skills ts \
               join agent_messages m on m.id = ts.reply_id \
                and m.created_at >= $2 and m.created_at < $3 \
               join skills s on s.id = ts.skill_id \
              where s.workspace_id = $1 \
                and ts.reply_id >= uuid7_floor($2 - interval '1 minute') and ts.reply_id < uuid7_floor($3 + interval '1 minute') \
              group by s.id, s.name, s.slug \
              order by turns desc, s.name \
              limit $4",
        )
        .bind(workspace_id)
        .bind(from)
        .bind(to)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        let used = used
            .iter()
            .map(|r| SkillUse {
                skill_id: r.get("id"),
                name: r.get("name"),
                slug: r.get("slug"),
                turns: r.get("turns"),
                sessions: r.get("sessions"),
                versions: r.get("versions"),
                last_used: r.get("last_used"),
            })
            .collect();

        // Carried and doing nothing. `last_used` reaches outside the window on
        // purpose: "not since March" and "never once" are different answers,
        // and a reader deciding whether to unbind something wants the second
        // one stated rather than inferred from a gap.
        let idle = sqlx::query(
            "select s.id, s.name, s.slug, \
                    count(distinct a.agent_id) as agents, \
                    (select m.created_at from turn_skills t \
                       join agent_messages m on m.id = t.reply_id \
                      where t.skill_id = s.id \
                      order by t.reply_id desc limit 1) as last_used \
               from skills s \
               join agent_skills a on a.skill_id = s.id \
              where s.workspace_id = $1 \
                and s.retired_at is null \
                and not exists ( \
                    select 1 from turn_skills ts \
                      join agent_messages m on m.id = ts.reply_id \
                     where ts.skill_id = s.id \
                       and ts.reply_id >= uuid7_floor($2 - interval '1 minute') and ts.reply_id < uuid7_floor($3 + interval '1 minute') \
                       and m.created_at >= $2 and m.created_at < $3) \
              group by s.id, s.name, s.slug \
              order by s.name \
              limit $4",
        )
        .bind(workspace_id)
        .bind(from)
        .bind(to)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        let idle = idle
            .iter()
            .map(|r| IdleSkill {
                skill_id: r.get("id"),
                name: r.get("name"),
                slug: r.get("slug"),
                agents: r.get("agents"),
                last_used: r.get("last_used"),
            })
            .collect();

        // Serving something older than the newest version. `pinned` is what
        // tells the two cases apart: an agent holding a version deliberately,
        // or an edit nobody has picked up. Both are worth seeing and only one
        // is worth acting on.
        let lagging = sqlx::query(
            "select s.id, s.name, s.slug, \
                    l.ordinal as latest, \
                    max(v.ordinal) as serving, \
                    count(*) as turns, \
                    bool_or(a.version_id is not null) as pinned \
               from turn_skills ts \
               join agent_messages m on m.id = ts.reply_id \
                and m.created_at >= $2 and m.created_at < $3 \
               join skills s on s.id = ts.skill_id \
               join skill_versions v on v.id = ts.version_id \
               cross join lateral ( \
                   select ordinal from skill_versions \
                    where skill_id = s.id order by ordinal desc limit 1) l \
               left join agent_skills a on a.skill_id = s.id \
              where s.workspace_id = $1 \
                and ts.reply_id >= uuid7_floor($2 - interval '1 minute') and ts.reply_id < uuid7_floor($3 + interval '1 minute') \
              group by s.id, s.name, s.slug, l.ordinal \
             having max(v.ordinal) < l.ordinal \
              order by turns desc, s.name \
              limit $4",
        )
        .bind(workspace_id)
        .bind(from)
        .bind(to)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        let lagging = lagging
            .iter()
            .map(|r| LaggingSkill {
                skill_id: r.get("id"),
                name: r.get("name"),
                slug: r.get("slug"),
                latest: r.get("latest"),
                serving: r.get("serving"),
                turns: r.get("turns"),
                // `bool_or` over no rows is null: a skill no agent carries.
                pinned: r.try_get("pinned").unwrap_or(false),
            })
            .collect();

        // Who wrote what. Left joined to the user, because a version written by
        // an install or a seed carries no author and dropping those rows would
        // make the counts here disagree with `totals.versions`. The operator's
        // staff are grouped as one, with no id, so they are neither named nor
        // told apart -- see `api::actor`.
        let authors = sqlx::query(concat!(
            "with authored as ( \
                 select v.skill_id, ",
            crate::operator_staff_sql!("v.created_by"),
            " as staff, v.created_by \
                   from skill_versions v \
                  where v.workspace_id = $1 \
                    and v.created_at >= $2 and v.created_at < $3 \
             ) \
             select case when a.staff then null else a.created_by end as user_id, \
                    a.staff, u.display_name as name, \
                    count(*) as versions, \
                    count(distinct a.skill_id) as skills \
               from authored a \
               left join users u on u.id = a.created_by and not a.staff \
              group by 1, a.staff, u.display_name \
              order by versions desc, name nulls last \
              limit $4",
        ))
        .bind(workspace_id)
        .bind(from)
        .bind(to)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        let authors = authors
            .iter()
            .map(|r| {
                let staff: bool = r.get("staff");
                SkillAuthor {
                    user_id: r.get("user_id"),
                    author: if staff {
                        Actor::Operator
                    } else {
                        Actor::user(r.get("name"), false)
                    },
                    versions: r.get("versions"),
                    skills: r.get("skills"),
                }
            })
            .collect();

        Ok(SkillStats {
            from,
            to,
            totals,
            used,
            idle,
            lagging,
            authors,
        })
    }
}

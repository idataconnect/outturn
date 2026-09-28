//! The skill figures, computed where the rows are.

use async_trait::async_trait;
use sqlx::{PgPool, Row};
use uuid::Uuid;

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
                    (select max(m.created_at) from turn_skills t \
                       join agent_messages m on m.id = t.reply_id \
                      where t.skill_id = s.id) as last_used \
               from skills s \
               join agent_skills a on a.skill_id = s.id \
              where s.workspace_id = $1 \
                and s.retired_at is null \
                and not exists ( \
                    select 1 from turn_skills ts \
                      join agent_messages m on m.id = ts.reply_id \
                     where ts.skill_id = s.id \
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
            "with latest as ( \
                 select skill_id, max(ordinal) as ordinal \
                   from skill_versions where workspace_id = $1 group by skill_id) \
             select s.id, s.name, s.slug, \
                    l.ordinal as latest, \
                    max(v.ordinal) as serving, \
                    count(*) as turns, \
                    bool_or(a.version_id is not null) as pinned \
               from turn_skills ts \
               join agent_messages m on m.id = ts.reply_id \
                and m.created_at >= $2 and m.created_at < $3 \
               join skills s on s.id = ts.skill_id \
               join skill_versions v on v.id = ts.version_id \
               join latest l on l.skill_id = s.id \
               left join agent_skills a on a.skill_id = s.id \
              where s.workspace_id = $1 \
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
        // make the counts here disagree with `totals.versions`.
        let authors = sqlx::query(
            "select v.created_by as user_id, u.display_name as name, \
                    count(*) as versions, \
                    count(distinct v.skill_id) as skills \
               from skill_versions v \
               left join users u on u.id = v.created_by \
              where v.workspace_id = $1 \
                and v.created_at >= $2 and v.created_at < $3 \
              group by v.created_by, u.display_name \
              order by versions desc, name nulls last \
              limit $4",
        )
        .bind(workspace_id)
        .bind(from)
        .bind(to)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        let authors = authors
            .iter()
            .map(|r| SkillAuthor {
                user_id: r.get("user_id"),
                name: r.get("name"),
                versions: r.get("versions"),
                skills: r.get("skills"),
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

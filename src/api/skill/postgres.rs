use async_trait::async_trait;
use sqlx::Row;
use sqlx::postgres::PgPool;
use uuid::Uuid;

use crate::api::usage::PLATFORM_WORKSPACE;

use super::{
    Binding, CreateSkill, ForkSkill, NewVersion, ResolvedSkill, Skill, SkillError, SkillKind,
    SkillStore, SkillVersion, UpdateSkill, validate_name, validate_slug,
};

pub struct PostgresSkillStore {
    pool: PgPool,
}

impl PostgresSkillStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Declared hosts go through the same normaliser a hand-written rule does, so
/// `https://api.example.com/v1/x` and `api.example.com` are one host and the
/// comparison against the egress rules is a string match rather than a guess.
fn clean_hosts(hosts: &[String]) -> Result<Vec<String>, SkillError> {
    let mut out: Vec<String> = Vec::new();
    for h in hosts {
        if h.trim().is_empty() {
            continue;
        }
        let host = crate::runtime::egress::normalise_host(h).map_err(SkillError::Invalid)?;
        if !out.contains(&host) {
            out.push(host);
        }
    }
    Ok(out)
}

async fn write_hosts(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    version_id: Uuid,
    hosts: &[String],
) -> Result<(), SkillError> {
    for host in hosts {
        sqlx::query("insert into skill_version_hosts (version_id, host) values ($1, $2)")
            .bind(version_id)
            .bind(host)
            .execute(&mut **tx)
            .await
            .map_err(internal)?;
    }
    Ok(())
}

fn internal(e: sqlx::Error) -> SkillError {
    SkillError::Internal(e.to_string())
}

/// Postgres reports a unique-constraint breach as SQLSTATE 23505. Two can reach
/// here: the slug, and the one override per base per workspace.
fn map_write_error(e: sqlx::Error, slug: &str) -> SkillError {
    if let sqlx::Error::Database(ref db) = e {
        match db.code().as_deref() {
            Some("23505") if db.constraint() == Some("skills_one_override_per_base_idx") => {
                return SkillError::Invalid(
                    "this workspace already overrides that skill; edit the override it has".into(),
                );
            }
            Some("23505") => return SkillError::DuplicateSlug(slug.to_string()),
            Some("23503") => {
                return SkillError::Invalid("that skill is still in use and cannot go".into());
            }
            _ => {}
        }
    }
    internal(e)
}

/// The columns every read returns, with the live version joined on and the
/// staleness of an override worked out beside it.
///
/// `base_moved` compares what this override was written against with what its
/// base is now. It is a read rather than a stored flag because the base moves
/// without touching the override, so anything written down would be wrong the
/// moment the operator saved.
macro_rules! select_skill {
    ($tail:literal) => {
        concat!(
            "select s.id, s.workspace_id, s.slug, s.name, s.description, s.kind, ",
            "s.base_skill_id, s.forked_from_skill_id, s.forked_from_version_id, ",
            "s.retired_at, v.id as version_id, v.ordinal, ",
            "coalesce((select array_agg(h.host order by h.host) ",
            "            from skill_version_hosts h where h.version_id = v.id), '{}') as hosts, ",
            "coalesce((select array_agg(h.host order by h.host) ",
            "            from skill_version_hosts h ",
            "           where h.version_id = v.id ",
            "             and not exists (select 1 from egress_rules e ",
            "                              where e.workspace_id = $2 and e.host = h.host and e.enabled)), ",
            "         '{}') as unmet_hosts, ",
            "coalesce( ",
            "    v.based_on_version_id is not null ",
            "    and v.based_on_version_id is distinct from ( ",
            "        select b.id from skill_versions b ",
            "         where b.skill_id = s.base_skill_id ",
            "         order by b.ordinal desc limit 1 ",
            "    ), false) as base_moved ",
            "  from skills s ",
            "  left join lateral ( ",
            "      select id, ordinal, based_on_version_id from skill_versions ",
            "       where skill_id = s.id order by ordinal desc limit 1 ",
            "  ) v on true ",
            $tail
        )
    };
}

fn read_skill(row: &sqlx::postgres::PgRow) -> Skill {
    Skill {
        id: row.get("id"),
        workspace_id: row.get("workspace_id"),
        slug: row.get("slug"),
        name: row.get("name"),
        description: row.get("description"),
        kind: SkillKind::parse(row.get::<String, _>("kind").as_str()),
        base_skill_id: row.get("base_skill_id"),
        forked_from_skill_id: row.get("forked_from_skill_id"),
        forked_from_version_id: row.get("forked_from_version_id"),
        retired_at: row.get("retired_at"),
        hosts: row.get("hosts"),
        unmet_hosts: row.get("unmet_hosts"),
        version_id: row.get("version_id"),
        ordinal: row.get("ordinal"),
        base_moved: row.get("base_moved"),
    }
}

fn read_version(row: &sqlx::postgres::PgRow) -> SkillVersion {
    SkillVersion {
        id: row.get("id"),
        skill_id: row.get("skill_id"),
        ordinal: row.get("ordinal"),
        body: row.get("body"),
        note: row.get("note"),
        based_on_version_id: row.get("based_on_version_id"),
        hosts: Vec::new(),
        created_by: row.get("created_by"),
        created_at: row.get("created_at"),
    }
}

#[async_trait]
impl SkillStore for PostgresSkillStore {
    async fn list(&self, workspace_id: Uuid) -> Result<Vec<Skill>, SkillError> {
        // The operator's skills read as though they were the workspace's own to
        // look at, because deciding whether to override one requires seeing it.
        let rows = sqlx::query(select_skill!(
            "where s.workspace_id = any($1) order by s.name"
        ))
        .bind(vec![workspace_id, PLATFORM_WORKSPACE])
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        Ok(rows.iter().map(read_skill).collect())
    }

    async fn get(&self, workspace_id: Uuid, id: Uuid) -> Result<Skill, SkillError> {
        let row = sqlx::query(select_skill!(
            "where s.workspace_id = any($1) and s.id = $3"
        ))
        .bind(vec![workspace_id, PLATFORM_WORKSPACE])
        .bind(workspace_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;
        row.as_ref().map(read_skill).ok_or(SkillError::NotFound)
    }

    async fn create(
        &self,
        workspace_id: Uuid,
        author: Uuid,
        input: CreateSkill,
    ) -> Result<Skill, SkillError> {
        validate_slug(&input.slug)?;
        validate_name(&input.name)?;

        let kind = if input.base_skill_id.is_some() {
            SkillKind::Override
        } else {
            SkillKind::Standalone
        };

        let mut tx = self.pool.begin().await.map_err(internal)?;

        // An override needs a base it can actually see, and one that is not
        // itself an override: overriding an override would make composition
        // order a question with no answer in the data.
        let based_on = match input.base_skill_id {
            Some(base) => {
                let row = sqlx::query(
                    "select s.kind, (select v.id from skill_versions v where v.skill_id = s.id \
                     order by v.ordinal desc limit 1) as live \
                     from skills s where s.id = $1 and s.workspace_id = any($2)",
                )
                .bind(base)
                .bind(vec![workspace_id, PLATFORM_WORKSPACE])
                .fetch_optional(&mut *tx)
                .await
                .map_err(internal)?
                .ok_or(SkillError::NotFound)?;

                if row.get::<String, _>("kind") == "override" {
                    return Err(SkillError::Invalid(
                        "an override cannot itself be overridden".into(),
                    ));
                }
                row.get::<Option<Uuid>, _>("live")
            }
            None => None,
        };

        let hosts = clean_hosts(&input.hosts)?;
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into skills (id, workspace_id, slug, name, description, kind, base_skill_id, created_by) \
             values ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(id)
        .bind(workspace_id)
        .bind(input.slug.trim())
        .bind(input.name.trim())
        .bind(&input.description)
        .bind(kind.as_str())
        .bind(input.base_skill_id)
        .bind(author)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_write_error(e, &input.slug))?;

        let first = Uuid::now_v7();
        sqlx::query(
            "insert into skill_versions (id, workspace_id, skill_id, ordinal, body, note, based_on_version_id, created_by) \
             values ($1, $2, $3, 1, $4, 'first version', $5, $6)",
        )
        .bind(first)
        .bind(workspace_id)
        .bind(id)
        .bind(&input.body)
        .bind(based_on)
        .bind(author)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
        write_hosts(&mut tx, first, &hosts).await?;

        tx.commit().await.map_err(internal)?;
        self.get(workspace_id, id).await
    }

    async fn update(
        &self,
        workspace_id: Uuid,
        id: Uuid,
        input: UpdateSkill,
    ) -> Result<Skill, SkillError> {
        if let Some(name) = &input.name {
            validate_name(name)?;
        }
        // Matched on the workspace's own id, so the operator's skill is not
        // found here rather than being refused: varying one means overriding it.
        let done = sqlx::query(
            "update skills set name = coalesce($3, name), \
             description = coalesce($4, description), updated_at = now() \
             where workspace_id = $1 and id = $2",
        )
        .bind(workspace_id)
        .bind(id)
        .bind(input.name.as_deref().map(str::trim))
        .bind(input.description.as_deref())
        .execute(&self.pool)
        .await
        .map_err(internal)?;

        if done.rows_affected() == 0 {
            return Err(SkillError::NotFound);
        }
        self.get(workspace_id, id).await
    }

    async fn add_version(
        &self,
        workspace_id: Uuid,
        id: Uuid,
        author: Uuid,
        input: NewVersion,
    ) -> Result<SkillVersion, SkillError> {
        let mut tx = self.pool.begin().await.map_err(internal)?;

        let skill = sqlx::query("select base_skill_id from skills where workspace_id = $1 and id = $2")
            .bind(workspace_id)
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(internal)?
            .ok_or(SkillError::NotFound)?;

        // An override's every version records the base it was written against,
        // so a later edit of the base can be reported against this one rather
        // than against whatever the override said when it was first written.
        let based_on: Option<Uuid> = match skill.get::<Option<Uuid>, _>("base_skill_id") {
            Some(base) => sqlx::query_scalar(
                "select id from skill_versions where skill_id = $1 order by ordinal desc limit 1",
            )
            .bind(base)
            .fetch_optional(&mut *tx)
            .await
            .map_err(internal)?
            .flatten(),
            None => None,
        };

        let hosts = clean_hosts(&input.hosts)?;
        let version_id = Uuid::now_v7();
        let row = sqlx::query(
            "insert into skill_versions (id, workspace_id, skill_id, ordinal, body, note, based_on_version_id, created_by) \
             values ($1, $2, $3, \
                     (select coalesce(max(ordinal), 0) + 1 from skill_versions where skill_id = $3), \
                     $4, $5, $6, $7) \
             returning id, skill_id, ordinal, body, note, based_on_version_id, created_by, created_at",
        )
        .bind(version_id)
        .bind(workspace_id)
        .bind(id)
        .bind(&input.body)
        .bind(&input.note)
        .bind(based_on)
        .bind(author)
        .fetch_one(&mut *tx)
        .await
        .map_err(internal)?;

        write_hosts(&mut tx, version_id, &hosts).await?;
        sqlx::query("update skills set updated_at = now() where id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(internal)?;

        tx.commit().await.map_err(internal)?;
        let mut version = read_version(&row);
        version.hosts = hosts;
        Ok(version)
    }

    async fn versions(&self, workspace_id: Uuid, id: Uuid) -> Result<Vec<SkillVersion>, SkillError> {
        self.get(workspace_id, id).await?;
        let rows = sqlx::query(
            "select id, skill_id, ordinal, body, note, based_on_version_id, created_by, created_at \
             from skill_versions where skill_id = $1 order by ordinal desc",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        Ok(rows.iter().map(read_version).collect())
    }

    async fn version(
        &self,
        workspace_id: Uuid,
        id: Uuid,
        version_id: Uuid,
    ) -> Result<SkillVersion, SkillError> {
        self.get(workspace_id, id).await?;
        let row = sqlx::query(
            "select id, skill_id, ordinal, body, note, based_on_version_id, created_by, created_at \
             from skill_versions where skill_id = $1 and id = $2",
        )
        .bind(id)
        .bind(version_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;
        row.as_ref().map(read_version).ok_or(SkillError::NotFound)
    }

    async fn fork(
        &self,
        workspace_id: Uuid,
        source_id: Uuid,
        author: Uuid,
        input: ForkSkill,
    ) -> Result<Skill, SkillError> {
        validate_slug(&input.slug)?;
        validate_name(&input.name)?;
        let source = self.get(workspace_id, source_id).await?;
        if source.kind == SkillKind::Override {
            return Err(SkillError::Invalid("an override cannot be forked".into()));
        }

        let taken = match input.version_id {
            Some(v) => self.version(workspace_id, source_id, v).await?,
            None => {
                let id = source.version_id.ok_or(SkillError::NotFound)?;
                self.version(workspace_id, source_id, id).await?
            }
        };

        let mut tx = self.pool.begin().await.map_err(internal)?;
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into skills (id, workspace_id, slug, name, description, kind, \
             forked_from_skill_id, forked_from_version_id, created_by) \
             values ($1, $2, $3, $4, $5, 'standalone', $6, $7, $8)",
        )
        .bind(id)
        .bind(workspace_id)
        .bind(input.slug.trim())
        .bind(input.name.trim())
        .bind(&source.description)
        .bind(source_id)
        .bind(taken.id)
        .bind(author)
        .execute(&mut *tx)
        .await
        .map_err(|e| map_write_error(e, &input.slug))?;

        sqlx::query(
            "insert into skill_versions (id, workspace_id, skill_id, ordinal, body, note, created_by) \
             values ($1, $2, $3, 1, $4, $5, $6)",
        )
        .bind(Uuid::now_v7())
        .bind(workspace_id)
        .bind(id)
        .bind(&taken.body)
        .bind(format!("forked from {} v{}", source.name, taken.ordinal))
        .bind(author)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;

        tx.commit().await.map_err(internal)?;
        self.get(workspace_id, id).await
    }

    async fn retire(
        &self,
        workspace_id: Uuid,
        id: Uuid,
        retired: bool,
    ) -> Result<Skill, SkillError> {
        let done = sqlx::query(
            "update skills set retired_at = case when $3 then now() else null end, \
             updated_at = now() where workspace_id = $1 and id = $2",
        )
        .bind(workspace_id)
        .bind(id)
        .bind(retired)
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        if done.rows_affected() == 0 {
            return Err(SkillError::NotFound);
        }
        self.get(workspace_id, id).await
    }

    async fn delete(&self, workspace_id: Uuid, id: Uuid) -> Result<(), SkillError> {
        let done = sqlx::query("delete from skills where workspace_id = $1 and id = $2")
            .bind(workspace_id)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| map_write_error(e, ""))?;
        if done.rows_affected() == 0 {
            return Err(SkillError::NotFound);
        }
        Ok(())
    }

    async fn bindings(&self, workspace_id: Uuid, agent_id: Uuid) -> Result<Vec<Binding>, SkillError> {
        let rows = sqlx::query(
            "select skill_id, version_id, position from agent_skills \
             where workspace_id = $1 and agent_id = $2 order by position",
        )
        .bind(workspace_id)
        .bind(agent_id)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        Ok(rows
            .iter()
            .map(|r| Binding {
                skill_id: r.get("skill_id"),
                version_id: r.get("version_id"),
                position: r.get("position"),
            })
            .collect())
    }

    async fn set_bindings(
        &self,
        workspace_id: Uuid,
        agent_id: Uuid,
        bindings: &[Binding],
    ) -> Result<(), SkillError> {
        let mut tx = self.pool.begin().await.map_err(internal)?;

        // Only standalone skills are bound. An override is the workspace's
        // standing variation and applies wherever its base does, so binding one
        // would be asking for it twice.
        for b in bindings {
            let kind: Option<String> = sqlx::query_scalar(
                "select kind from skills where id = $1 and workspace_id = any($2) \
                 and retired_at is null",
            )
            .bind(b.skill_id)
            .bind(vec![workspace_id, PLATFORM_WORKSPACE])
            .fetch_optional(&mut *tx)
            .await
            .map_err(internal)?;

            match kind.as_deref() {
                None => return Err(SkillError::NotFound),
                Some("override") => {
                    return Err(SkillError::Invalid(
                        "an override applies wherever its base is used and is not bound on its own"
                            .into(),
                    ));
                }
                Some(_) => {}
            }
        }

        // Checked here because binding is the moment a skill becomes something a
        // turn will actually run. The overrides that ride along are included:
        // they compose with the base whether or not anybody bound them, so
        // their declarations count the same.
        let ids: Vec<Uuid> = bindings.iter().map(|b| b.skill_id).collect();
        let unmet: Vec<String> = sqlx::query_scalar(
            "select distinct h.host \
               from skill_version_hosts h \
               join skill_versions v on v.id = h.version_id \
               join skills s on s.id = v.skill_id \
              where v.ordinal = (select max(ordinal) from skill_versions where skill_id = s.id) \
                and (s.id = any($2) \
                     or (s.workspace_id = $1 and s.kind = 'override' \
                         and s.base_skill_id = any($2))) \
                and not exists (select 1 from egress_rules e \
                                 where e.workspace_id = $1 and e.host = h.host and e.enabled) \
              order by h.host",
        )
        .bind(workspace_id)
        .bind(&ids)
        .fetch_all(&mut *tx)
        .await
        .map_err(internal)?;

        if !unmet.is_empty() {
            // Refused rather than queued. When there is somewhere to send a
            // request for access, this is the branch that raises it -- the
            // hosts are already in hand, and the approver is whoever may write
            // an egress rule.
            return Err(SkillError::HostsNotAllowed(unmet));
        }

        sqlx::query("delete from agent_skills where workspace_id = $1 and agent_id = $2")
            .bind(workspace_id)
            .bind(agent_id)
            .execute(&mut *tx)
            .await
            .map_err(internal)?;

        for (i, b) in bindings.iter().enumerate() {
            sqlx::query(
                "insert into agent_skills (workspace_id, agent_id, skill_id, version_id, position) \
                 values ($1, $2, $3, $4, $5)",
            )
            .bind(workspace_id)
            .bind(agent_id)
            .bind(b.skill_id)
            .bind(b.version_id)
            .bind(i as i32)
            .execute(&mut *tx)
            .await
            .map_err(internal)?;
        }

        tx.commit().await.map_err(internal)?;
        Ok(())
    }

    async fn resolve_for_agent(
        &self,
        workspace_id: Uuid,
        agent_id: Uuid,
    ) -> Result<Vec<ResolvedSkill>, SkillError> {
        // Two passes over the same bindings: the skills themselves, then this
        // workspace's overrides of them. `tier` is what puts an override after
        // the prose it speaks about, which is the whole of how it takes
        // precedence -- a model reads the later instruction as the current one.
        let rows = sqlx::query(
            "with bound as (
                 select b.skill_id, b.version_id as pinned, b.position
                   from agent_skills b
                  where b.workspace_id = $1 and b.agent_id = $2
             ),
             picked as (
                 select bd.position, 0 as tier, s.id as skill_id, s.name, s.kind,
                        coalesce(bd.pinned, (select v.id from skill_versions v
                                              where v.skill_id = s.id
                                              order by v.ordinal desc limit 1)) as version_id
                   from bound bd
                   join skills s on s.id = bd.skill_id
                  where s.retired_at is null
                 union all
                 select bd.position, 1 as tier, o.id, o.name, o.kind,
                        (select v.id from skill_versions v where v.skill_id = o.id
                          order by v.ordinal desc limit 1)
                   from bound bd
                   join skills o on o.base_skill_id = bd.skill_id
                  where o.workspace_id = $1 and o.kind = 'override' and o.retired_at is null
             )
             select p.position, p.tier, p.skill_id, p.version_id, p.name, p.kind, v.body
               from picked p
               join skill_versions v on v.id = p.version_id
              order by p.position, p.tier",
        )
        .bind(workspace_id)
        .bind(agent_id)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        Ok(rows
            .iter()
            .enumerate()
            .map(|(i, r)| ResolvedSkill {
                skill_id: r.get("skill_id"),
                version_id: r.get("version_id"),
                name: r.get("name"),
                kind: SkillKind::parse(r.get::<String, _>("kind").as_str()),
                body: r.get("body"),
                position: i as i32,
            })
            .collect())
    }

    async fn approve_hosts(
        &self,
        workspace_id: Uuid,
        skill_id: Uuid,
        actor: Uuid,
    ) -> Result<Vec<String>, SkillError> {
        // Visible to this workspace, which is what lets it approve the hosts of
        // the operator's skill without being able to edit it.
        self.get(workspace_id, skill_id).await?;

        // `do nothing` rather than an error on conflict: a host somebody
        // already allowed is not a failure, it is the case where there was
        // nothing left to approve. What comes back is what actually opened.
        let opened: Vec<String> = sqlx::query_scalar(
            "insert into egress_rules (id, workspace_id, host, from_skill_id) \
             select uuidv7(), $1, h.host, $2 \
               from skill_version_hosts h \
               join skill_versions v on v.id = h.version_id \
              where v.skill_id = $2 \
                and v.ordinal = (select max(ordinal) from skill_versions where skill_id = $2) \
             on conflict (workspace_id, host) do nothing \
             returning host",
        )
        .bind(workspace_id)
        .bind(skill_id)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        if !opened.is_empty() {
            tracing::info!(
                actor = %actor,
                workspace_id = %workspace_id,
                skill_id = %skill_id,
                hosts = %opened.join(", "),
                "network access approved for a skill"
            );
        }
        Ok(opened)
    }

    async fn record_turn(
        &self,
        reply_id: Uuid,
        skills: &[ResolvedSkill],
    ) -> Result<(), SkillError> {
        for s in skills {
            sqlx::query(
                "insert into turn_skills (reply_id, skill_id, version_id, position) \
                 values ($1, $2, $3, $4) on conflict do nothing",
            )
            .bind(reply_id)
            .bind(s.skill_id)
            .bind(s.version_id)
            .bind(s.position)
            .execute(&self.pool)
            .await
            .map_err(internal)?;
        }
        Ok(())
    }
}

use async_trait::async_trait;
use sqlx::Row;
use sqlx::postgres::{PgPool, PgRow};
use uuid::Uuid;

/// A query built from constant pieces -- the column lists below -- and never
/// from anything a caller sent, which is what `AssertSqlSafe` asks to be told.
macro_rules! sql {
    ($($t:tt)*) => {
        sqlx::AssertSqlSafe(format!($($t)*))
    };
}

use super::{
    AgentTemplateStore, Availability, CatalogEntry, NewTemplate, NewVersion, Template,
    TemplateError, TemplateSkill, TemplateVersion, UpdateTemplate, validate_slug,
};
use crate::api::usage::PLATFORM_WORKSPACE;

pub struct PostgresAgentTemplateStore {
    pool: PgPool,
}

impl PostgresAgentTemplateStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn internal(e: sqlx::Error) -> TemplateError {
    TemplateError::Internal(e.to_string())
}

const VERSION_COLUMNS: &str = "v.id, v.template_id, v.ordinal, v.name, v.description, \
     v.requirements, v.defaults, v.reminder, v.policy, v.eager_tools, v.settings, v.note, \
     v.created_at";

fn read_version(row: &PgRow) -> TemplateVersion {
    TemplateVersion {
        id: row.get("id"),
        template_id: row.get("template_id"),
        ordinal: row.get("ordinal"),
        name: row.get("name"),
        description: row.get("description"),
        requirements: row.get("requirements"),
        defaults: row.get("defaults"),
        reminder: row.get("reminder"),
        policy: row.get("policy"),
        eager_tools: row.get("eager_tools"),
        skills: Vec::new(),
        settings: match row.get::<serde_json::Value, _>("settings") {
            serde_json::Value::Object(map) => map,
            _ => Default::default(),
        },
        note: row.get("note"),
        created_at: row.get("created_at"),
    }
}

/// Every template with its newest version, filtered by `filter` on `t`.
fn templates_query(filter: &str) -> String {
    format!(
        "select t.id as tid, t.slug, t.availability, t.allow_additions, \
                t.retired_at is not null as retired, {VERSION_COLUMNS} \
           from agent_templates t \
           join lateral ( \
               select * from agent_template_versions \
                where template_id = t.id order by ordinal desc limit 1 \
           ) v on true \
          where {filter} \
          order by t.slug"
    )
}

impl PostgresAgentTemplateStore {
    /// Fills in each version's skills, in one query.
    async fn with_skills(
        &self,
        mut versions: Vec<TemplateVersion>,
    ) -> Result<Vec<TemplateVersion>, TemplateError> {
        let ids: Vec<Uuid> = versions.iter().map(|v| v.id).collect();
        let rows = sqlx::query(
            "select template_version_id, skill_id, version_id from agent_template_skills \
              where template_version_id = any($1) order by position",
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        for v in &mut versions {
            v.skills = rows
                .iter()
                .filter(|r| r.get::<Uuid, _>("template_version_id") == v.id)
                .map(|r| TemplateSkill {
                    skill_id: r.get("skill_id"),
                    version_id: r.get("version_id"),
                })
                .collect();
        }
        Ok(versions)
    }

    async fn templates(
        &self,
        filter: &str,
        id: Option<Uuid>,
    ) -> Result<Vec<Template>, TemplateError> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(templates_query(filter)))
            .bind(id)
            .fetch_all(&self.pool)
            .await
            .map_err(internal)?;
        let versions = self
            .with_skills(rows.iter().map(read_version).collect())
            .await?;
        Ok(rows
            .iter()
            .zip(versions)
            .map(|(r, current)| Template {
                id: r.get("tid"),
                slug: r.get("slug"),
                availability: Availability::parse(r.get::<String, _>("availability").as_str())
                    .unwrap_or(Availability::Optional),
                allow_additions: r.get("allow_additions"),
                retired: r.get("retired"),
                current,
            })
            .collect())
    }

    /// Writes a version and its skills inside `tx`.
    async fn write_version(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        template_id: Uuid,
        input: &NewVersion,
        created_by: Option<Uuid>,
    ) -> Result<(), TemplateError> {
        if input.name.trim().is_empty() {
            return Err(TemplateError::Invalid("a template needs a name".into()));
        }
        // Operator skills only: a template is in every workspace, and a skill
        // from one of them would be that workspace's in all the others.
        let skill_ids: Vec<Uuid> = input.skills.iter().map(|s| s.skill_id).collect();
        let platform: i64 = sqlx::query_scalar(
            "select count(*) from skills where id = any($1) and workspace_id = $2",
        )
        .bind(&skill_ids)
        .bind(PLATFORM_WORKSPACE)
        .fetch_one(&mut **tx)
        .await
        .map_err(internal)?;
        if platform as usize != skill_ids.len() {
            return Err(TemplateError::Invalid(
                "a template's skills must be the operator's own".into(),
            ));
        }

        // Checked against the catalog as an override would be: a template
        // fixing a value no setting can take would fail every turn instead.
        for (key, value) in &input.settings {
            let setting = crate::api::settings::find(key)
                .ok_or_else(|| TemplateError::Invalid(format!("no setting named {key}")))?;
            crate::api::settings::validate(&setting, value)
                .map_err(|e| TemplateError::Invalid(e.to_string()))?;
        }

        let version_id = Uuid::now_v7();
        sqlx::query(
            "insert into agent_template_versions \
                 (id, template_id, ordinal, name, description, requirements, defaults, \
                  reminder, policy, eager_tools, settings, note, created_by) \
             values ($1, $2, \
                     (select coalesce(max(ordinal), 0) + 1 from agent_template_versions \
                       where template_id = $2), \
                     $3, $4, $5, $6, $7, coalesce($8, '{}'::jsonb), $9, $10, $11, $12)",
        )
        .bind(version_id)
        .bind(template_id)
        .bind(input.name.trim())
        .bind(input.description.trim())
        .bind(&input.requirements)
        .bind(&input.defaults)
        .bind(&input.reminder)
        .bind(&input.policy)
        .bind(&input.eager_tools)
        .bind(serde_json::Value::Object(input.settings.clone()))
        .bind(&input.note)
        .bind(created_by)
        .execute(&mut **tx)
        .await
        .map_err(internal)?;
        for (position, skill) in input.skills.iter().enumerate() {
            sqlx::query(
                "insert into agent_template_skills \
                     (template_version_id, skill_id, version_id, position) \
                 values ($1, $2, $3, $4)",
            )
            .bind(version_id)
            .bind(skill.skill_id)
            .bind(skill.version_id)
            .bind(position as i32)
            .execute(&mut **tx)
            .await
            .map_err(internal)?;
        }
        sqlx::query("update agent_templates set updated_at = now() where id = $1")
            .bind(template_id)
            .execute(&mut **tx)
            .await
            .map_err(internal)?;
        Ok(())
    }

    /// Makes a workspace's agent from a template, or finds the one it has.
    ///
    /// Named by the template's slug, or the slug with a number after it where
    /// the workspace already made an agent by that name itself: the template's
    /// agent should not displace one the workspace owns.
    async fn make_agent(
        &self,
        workspace_id: Uuid,
        template_id: Uuid,
    ) -> Result<(Uuid, bool), TemplateError> {
        if let Some(id) = sqlx::query_scalar::<_, Uuid>(
            "select id from agents where workspace_id = $1 and template_id = $2",
        )
        .bind(workspace_id)
        .bind(template_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?
        {
            return Ok((id, false));
        }
        let row = sqlx::query(
            "select t.slug, v.name, v.description from agent_templates t \
               join lateral (select * from agent_template_versions \
                              where template_id = t.id order by ordinal desc limit 1) v on true \
              where t.id = $1",
        )
        .bind(template_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?
        .ok_or(TemplateError::NotFound)?;
        let base: String = row.get("slug");
        for n in 1..=20 {
            let slug = if n == 1 {
                base.clone()
            } else {
                format!("{base}-{n}")
            };
            let id = Uuid::now_v7();
            let made = sqlx::query(
                "insert into agents (id, workspace_id, name, slug, description, template_id) \
                 values ($1, $2, $3, $4, $5, $6)",
            )
            .bind(id)
            .bind(workspace_id)
            .bind(row.get::<String, _>("name"))
            .bind(&slug)
            .bind(row.get::<String, _>("description"))
            .bind(template_id)
            .execute(&self.pool)
            .await;
            match made {
                Ok(_) => return Ok((id, true)),
                Err(sqlx::Error::Database(db)) if db.code().as_deref() == Some("23505") => {
                    // Somebody made it at the same moment: theirs is the one.
                    if db.constraint() == Some("agents_one_per_template") {
                        return Box::pin(self.make_agent(workspace_id, template_id)).await;
                    }
                    continue;
                }
                Err(e) => return Err(internal(e)),
            }
        }
        Err(TemplateError::Refused(format!(
            "this workspace already has agents named {base} through {base}-20"
        )))
    }
}

#[async_trait]
impl AgentTemplateStore for PostgresAgentTemplateStore {
    async fn list(&self) -> Result<Vec<Template>, TemplateError> {
        self.templates("$1::uuid is null", None).await
    }

    async fn get(&self, id: Uuid) -> Result<Template, TemplateError> {
        self.templates("t.id = $1", Some(id))
            .await?
            .into_iter()
            .next()
            .ok_or(TemplateError::NotFound)
    }

    async fn create(
        &self,
        input: NewTemplate,
        created_by: Option<Uuid>,
    ) -> Result<Template, TemplateError> {
        validate_slug(&input.slug)?;
        let id = Uuid::now_v7();
        let mut tx = self.pool.begin().await.map_err(internal)?;
        sqlx::query(
            "insert into agent_templates (id, slug, availability, allow_additions) \
             values ($1, $2, $3, $4)",
        )
        .bind(id)
        .bind(&input.slug)
        .bind(input.availability.as_str())
        .bind(input.allow_additions)
        .execute(&mut *tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.code().as_deref() == Some("23505") => {
                TemplateError::DuplicateSlug(input.slug.clone())
            }
            _ => internal(e),
        })?;
        Self::write_version(&mut tx, id, &input.version, created_by).await?;
        tx.commit().await.map_err(internal)?;
        if input.availability.provisioned() {
            self.provision(None).await?;
        }
        self.get(id).await
    }

    async fn publish(
        &self,
        id: Uuid,
        input: NewVersion,
        created_by: Option<Uuid>,
    ) -> Result<Template, TemplateError> {
        let template = self.get(id).await?;
        if template.retired {
            return Err(TemplateError::Refused(
                "a retired template is not published to; bring it back first".into(),
            ));
        }
        let mut tx = self.pool.begin().await.map_err(internal)?;
        Self::write_version(&mut tx, id, &input, created_by).await?;
        tx.commit().await.map_err(internal)?;
        // Renames reach the agents made from it; nothing else is copied, since
        // a turn reads the rest from the template itself.
        self.provision(None).await?;
        self.get(id).await
    }

    async fn update(&self, id: Uuid, input: UpdateTemplate) -> Result<Template, TemplateError> {
        let updated = sqlx::query(
            "update agent_templates set \
                 availability = coalesce($2, availability), \
                 allow_additions = coalesce($3, allow_additions), \
                 retired_at = case when $4 is null then retired_at \
                                   when $4 then coalesce(retired_at, now()) \
                                   else null end, \
                 updated_at = now() \
             where id = $1",
        )
        .bind(id)
        .bind(input.availability.map(Availability::as_str))
        .bind(input.allow_additions)
        .bind(input.retired)
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        if updated.rows_affected() == 0 {
            return Err(TemplateError::NotFound);
        }
        let template = self.get(id).await?;
        if template.availability.provisioned() && !template.retired {
            self.provision(None).await?;
        }
        Ok(template)
    }

    async fn versions(&self, id: Uuid) -> Result<Vec<TemplateVersion>, TemplateError> {
        self.get(id).await?;
        let rows = sqlx::query(sql!(
            "select {VERSION_COLUMNS} from agent_template_versions v \
              where v.template_id = $1 order by v.ordinal desc"
        ))
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        self.with_skills(rows.iter().map(read_version).collect())
            .await
    }

    async fn current_for_turn(&self, id: Uuid) -> Result<Option<TemplateVersion>, TemplateError> {
        // The newest version whether or not the template is retired: nothing
        // is published to a retired one, so its agents run as they last did.
        let row = sqlx::query(sql!(
            "select {VERSION_COLUMNS} from agent_template_versions v \
              where v.template_id = $1 order by v.ordinal desc limit 1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;
        match row {
            Some(row) => Ok(self
                .with_skills(vec![read_version(&row)])
                .await?
                .into_iter()
                .next()),
            None => Ok(None),
        }
    }

    async fn catalog(&self, workspace_id: Uuid) -> Result<Vec<CatalogEntry>, TemplateError> {
        let rows = sqlx::query(
            "select t.id, t.slug, t.availability, v.name, v.description, a.id as agent_id \
               from agent_templates t \
               join lateral (select * from agent_template_versions \
                              where template_id = t.id order by ordinal desc limit 1) v on true \
               left join agents a on a.template_id = t.id and a.workspace_id = $1 \
              where t.retired_at is null \
              order by v.name",
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        Ok(rows
            .iter()
            .map(|r| CatalogEntry {
                template_id: r.get("id"),
                slug: r.get("slug"),
                name: r.get("name"),
                description: r.get("description"),
                availability: Availability::parse(r.get::<String, _>("availability").as_str())
                    .unwrap_or(Availability::Optional),
                agent_id: r.get("agent_id"),
            })
            .collect())
    }

    async fn provision(&self, workspace_id: Option<Uuid>) -> Result<usize, TemplateError> {
        // Owed: every workspace but the platform's, every required template,
        // and every default one the workspace has not removed.
        let owed = sqlx::query(
            "select w.id as workspace_id, t.id as template_id \
               from workspaces w \
               cross join agent_templates t \
              where w.id <> $1 \
                and ($2::uuid is null or w.id = $2) \
                and t.retired_at is null \
                and (t.availability = 'required' \
                     or (t.availability = 'default' and not exists ( \
                         select 1 from agent_template_dismissals d \
                          where d.workspace_id = w.id and d.template_id = t.id))) \
                and not exists (select 1 from agents a \
                                 where a.workspace_id = w.id and a.template_id = t.id)",
        )
        .bind(PLATFORM_WORKSPACE)
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        let mut made = 0;
        for row in &owed {
            let (_, new) = self
                .make_agent(row.get("workspace_id"), row.get("template_id"))
                .await?;
            made += new as usize;
        }

        // Names follow the template, so a rename reaches every agent made
        // from it rather than leaving each workspace with the old one.
        sqlx::query(
            "update agents a set name = v.name, description = v.description, updated_at = now() \
               from agent_templates t \
               join lateral (select * from agent_template_versions \
                              where template_id = t.id order by ordinal desc limit 1) v on true \
              where a.template_id = t.id \
                and ($1::uuid is null or a.workspace_id = $1) \
                and (a.name <> v.name or a.description <> v.description)",
        )
        .bind(workspace_id)
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(made)
    }

    async fn install(&self, workspace_id: Uuid, template_id: Uuid) -> Result<Uuid, TemplateError> {
        let template = self.get(template_id).await?;
        if template.retired {
            return Err(TemplateError::NotFound);
        }
        sqlx::query(
            "delete from agent_template_dismissals where workspace_id = $1 and template_id = $2",
        )
        .bind(workspace_id)
        .bind(template_id)
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(self.make_agent(workspace_id, template_id).await?.0)
    }

    async fn removing(&self, workspace_id: Uuid, template_id: Uuid) -> Result<(), TemplateError> {
        let template = self.get(template_id).await?;
        match template.availability {
            Availability::Required if !template.retired => Err(TemplateError::Refused(
                "this agent is one every workspace has, and cannot be removed".into(),
            )),
            Availability::Default => {
                sqlx::query(
                    "insert into agent_template_dismissals (workspace_id, template_id) \
                     values ($1, $2) on conflict do nothing",
                )
                .bind(workspace_id)
                .bind(template_id)
                .execute(&self.pool)
                .await
                .map_err(internal)?;
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

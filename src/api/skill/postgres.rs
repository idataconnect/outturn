use async_trait::async_trait;
use sqlx::Row;
use sqlx::postgres::PgPool;
use uuid::Uuid;

use crate::api::usage::PLATFORM_WORKSPACE;

use super::{
    Binding, CreateSkill, DeclaredGate, ForkSkill, NewVersion, ResolvedSkill, Skill, SkillError,
    SkillFile, SkillKind, SkillStore, SkillVersion, UpdateSkill, validate_name, validate_slug,
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

/// Records what a version's files declare needs approving.
///
/// Derived here, where the content is in hand, rather than when a turn runs: a
/// file's content lives in the object store by hash, so a turn computing this
/// would read every bound skill's every file before its first token. A version is
/// immutable, so what it declares cannot change after this.
///
/// The host comes from the version's own declared hosts. A skill declaring one
/// host gates that host; a skill declaring several gates the request shape on each
/// of them, because the file says "POST /charges" and not which of its hosts --
/// and gating all of them is the direction that refuses too much rather than too
/// little.
async fn write_gates(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    version_id: Uuid,
    hosts: &[String],
    gates: &[DeclaredGate],
) -> Result<(), SkillError> {
    // A declaration with no host to hang it on stores nothing, and a version that
    // stored nothing is a version the gateway cannot gate -- while its file says
    // the operation is gated, and a reader believes it. Refused rather than
    // written, because the alternative is the quiet failure this whole mechanism
    // is arranged against, and because the fix is one line in the publish: say
    // which host the operation is on.
    if !gates.is_empty() && hosts.is_empty() {
        let paths: Vec<&str> = gates.iter().map(|g| g.path.as_str()).collect();
        return Err(SkillError::Invalid(format!(
            "{} declares an approval but this version names no host, so nothing \
             could enforce it; add the host the operation is on",
            paths.join(", ")
        )));
    }

    for rule in gates {
        for host in hosts {
            sqlx::query(
                "insert into skill_version_gates \
                 (version_id, path, requires, host, method, path_pattern, identified_by) \
                 values ($1, $2, $3, $4, $5, $6, $7) on conflict do nothing",
            )
            .bind(version_id)
            .bind(&rule.path)
            .bind(&rule.requires)
            .bind(host)
            .bind(&rule.method)
            .bind(&rule.path_pattern)
            .bind(rule.identified_by.as_deref())
            .execute(&mut **tx)
            .await
            .map_err(internal)?;

            // The bound fields, in declared order. Not keyed on the host --
            // what a request binds does not vary by where it is sent -- so the
            // repeats past the first host are no-ops through `on conflict`.
            for (position, field) in rule.binds.iter().enumerate() {
                sqlx::query(
                    "insert into skill_version_gate_binds \
                     (version_id, path, requires, position, field) \
                     values ($1, $2, $3, $4, $5) on conflict do nothing",
                )
                .bind(version_id)
                .bind(&rule.path)
                .bind(&rule.requires)
                .bind(position as i32)
                .bind(field)
                .execute(&mut **tx)
                .await
                .map_err(internal)?;
            }
        }
    }
    Ok(())
}

/// Copies a version's gates onto a new version.
///
/// For a fork and for a body-only edit, which carry files forward without their
/// content: the declaration belongs to the files, so it travels with them.
async fn copy_gates(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    from: Uuid,
    to: Uuid,
) -> Result<(), SkillError> {
    sqlx::query(
        "insert into skill_version_gates \
         (version_id, path, requires, host, method, path_pattern, identified_by) \
         select $2, path, requires, host, method, path_pattern, identified_by \
         from skill_version_gates where version_id = $1 \
         on conflict do nothing",
    )
    .bind(from)
    .bind(to)
    .execute(&mut **tx)
    .await
    .map_err(internal)?;

    // And what each of them binds. Carried with the gate rather than left
    // behind: a gate whose binds did not travel is one whose grants cover every
    // request to its path, so losing them here is the fail-open direction. The
    // body-only edit already lost a whole gate set once this way.
    sqlx::query(
        "insert into skill_version_gate_binds \
         (version_id, path, requires, position, field) \
         select $2, path, requires, position, field \
         from skill_version_gate_binds where version_id = $1 \
         on conflict do nothing",
    )
    .bind(from)
    .bind(to)
    .execute(&mut **tx)
    .await
    .map_err(internal)?;
    Ok(())
}

async fn write_files(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    version_id: Uuid,
    files: &[SkillFile],
) -> Result<(), SkillError> {
    for f in files {
        sqlx::query(
            "insert into skill_version_files (version_id, path, sha256, bytes, links) \
             values ($1, $2, $3, $4, $5)",
        )
        .bind(version_id)
        .bind(&f.path)
        .bind(&f.sha256)
        .bind(f.bytes)
        .bind(&f.links)
        .execute(&mut **tx)
        .await
        .map_err(internal)?;
    }
    Ok(())
}

async fn files_of<'e, E>(executor: E, version_id: Uuid) -> Result<Vec<SkillFile>, SkillError>
where
    E: sqlx::PgExecutor<'e>,
{
    let rows = sqlx::query(
        "select path, sha256, bytes, links from skill_version_files where version_id = $1 order by path",
    )
    .bind(version_id)
    .fetch_all(executor)
    .await
    .map_err(internal)?;
    Ok(rows
        .iter()
        .map(|r| SkillFile {
            path: r.get("path"),
            sha256: r.get("sha256"),
            bytes: r.get("bytes"),
            links: r.get("links"),
        })
        .collect())
}

async fn hosts_of<'e, E>(executor: E, version_id: Uuid) -> Result<Vec<String>, SkillError>
where
    E: sqlx::PgExecutor<'e>,
{
    sqlx::query_scalar("select host from skill_version_hosts where version_id = $1 order by host")
        .bind(version_id)
        .fetch_all(executor)
        .await
        .map_err(internal)
}

/// Files are prose about the base's API in an override's hands, and an
/// override speaks about its base rather than replacing any of it.
fn refuse_override_files(is_override: bool, files: &[SkillFile]) -> Result<(), SkillError> {
    if is_override && !files.is_empty() {
        return Err(SkillError::Invalid(
            "an override cannot carry files; it speaks about its base's".into(),
        ));
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
        files: Vec::new(),
        unreached: None,
        created_by: row.get("created_by"),
        created_at: row.get("created_at"),
    }
}

#[async_trait]
impl SkillStore for PostgresSkillStore {
    async fn list(
        &self,
        workspace_id: Uuid,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<Skill>, SkillError> {
        // The operator's skills read as though they were the workspace's own to
        // look at, because deciding whether to override one requires seeing it.
        let rows = sqlx::query(select_skill!(
            "where s.workspace_id = any($1) and ($3::uuid is null or s.id > $3) order by s.id limit $4"
        ))
        .bind(vec![workspace_id, PLATFORM_WORKSPACE])
        .bind(workspace_id)
        .bind(after)
        .bind(limit)
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
        files: &[SkillFile],
        gates: &[DeclaredGate],
    ) -> Result<Skill, SkillError> {
        validate_slug(&input.slug)?;
        validate_name(&input.name)?;

        let kind = if input.base_skill_id.is_some() {
            SkillKind::Override
        } else {
            SkillKind::Standalone
        };
        refuse_override_files(kind == SkillKind::Override, files)?;

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
        write_files(&mut tx, first, files).await?;
        write_gates(&mut tx, first, &hosts, gates).await?;

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
        files: Option<&[SkillFile]>,
        gates: &[DeclaredGate],
    ) -> Result<(SkillVersion, bool), SkillError> {
        let mut tx = self.pool.begin().await.map_err(internal)?;

        let skill =
            sqlx::query("select base_skill_id from skills where workspace_id = $1 and id = $2")
                .bind(workspace_id)
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(internal)?
                .ok_or(SkillError::NotFound)?;
        let base: Option<Uuid> = skill.get("base_skill_id");

        // An override's every version records the base it was written against,
        // so a later edit of the base can be reported against this one rather
        // than against whatever the override said when it was first written.
        let based_on: Option<Uuid> = match base {
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

        let live = sqlx::query(
            "select id, skill_id, ordinal, body, note, based_on_version_id, created_by, created_at \
             from skill_versions where skill_id = $1 order by ordinal desc limit 1",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(internal)?;

        let hosts = clean_hosts(&input.hosts)?;
        // Whether this version brings files of its own, which decides whether its
        // declarations are the ones given or the previous version's carried
        // forward.
        let files_given = files.is_some();
        let files: Vec<SkillFile> = match (files, &live) {
            (Some(f), _) => f.to_vec(),
            (None, Some(live)) => files_of(&mut *tx, live.get("id")).await?,
            (None, None) => Vec::new(),
        };
        refuse_override_files(base.is_some(), &files)?;

        if let Some(live) = &live {
            let live_id: Uuid = live.get("id");
            let live_hosts = hosts_of(&mut *tx, live_id).await?;
            let mut new_hosts = hosts.clone();
            new_hosts.sort();
            let live_files = files_of(&mut *tx, live_id).await?;
            if live.get::<String, _>("body") == input.body
                && live_hosts == new_hosts
                && live_files == files
                && live.get::<Option<Uuid>, _>("based_on_version_id") == based_on
            {
                let mut version = read_version(live);
                version.hosts = live_hosts;
                version.files = live_files;
                return Ok((version.with_unreached(), false));
            }
        }

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
        write_files(&mut tx, version_id, &files).await?;
        // New files bring their own declarations; a version carrying the previous
        // one's files carries its gates with them.
        //
        // From `live` -- this skill's latest version -- and not from `based_on`,
        // which is the latest version of the *base* an override speaks about and is
        // null for every ordinary skill. Keyed on `based_on`, neither branch ran on
        // an ordinary body-only edit: the files carried forward with their
        // frontmatter intact and the new version had no gates at all, so
        // `gates_for_turn` returned the empty set and every turn after somebody
        // fixed a typo in a skill's prose was ungated. Nothing logged it and nothing
        // refused it, which is the quiet failure `write_gates` exists to prevent --
        // and `write_gates`'s own guard could not fire, because it is not reached
        // when there are no gates to write.
        if files_given {
            write_gates(&mut tx, version_id, &hosts, gates).await?;
        } else if let Some(live) = &live {
            copy_gates(&mut tx, live.get("id"), version_id).await?;
        }
        sqlx::query("update skills set updated_at = now() where id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(internal)?;

        tx.commit().await.map_err(internal)?;
        let mut version = read_version(&row);
        version.hosts = hosts;
        version.files = files;
        Ok((version.with_unreached(), true))
    }

    async fn versions(
        &self,
        workspace_id: Uuid,
        id: Uuid,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<SkillVersion>, SkillError> {
        self.get(workspace_id, id).await?;
        let rows = sqlx::query(
            "select v.id, v.skill_id, v.ordinal, v.body, v.note, v.based_on_version_id, \
                    v.created_by, v.created_at, \
                    h.hosts, f.files, f.file_sizes, f.file_hashes, f.file_links \
             from skill_versions v \
             left join lateral ( \
                 select coalesce(array_agg(host order by host), '{}') as hosts \
                 from skill_version_hosts where version_id = v.id \
             ) h on true \
             left join lateral ( \
                 select coalesce(array_agg(path order by path), '{}') as files, \
                        coalesce(array_agg(bytes order by path), '{}') as file_sizes, \
                        coalesce(array_agg(sha256 order by path), '{}') as file_hashes, \
                        coalesce(jsonb_agg(links order by path), '[]') as file_links \
                 from skill_version_files where version_id = v.id \
             ) f on true \
             where v.skill_id = $1 \
               and ($2::uuid is null or v.id < $2) \
             order by v.id desc \
             limit $3",
        )
        .bind(id)
        .bind(after)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        Ok(rows
            .iter()
            .map(|r| {
                let paths: Vec<String> = r.get("files");
                let hashes: Vec<String> = r.get("file_hashes");
                let sizes: Vec<i32> = r.get("file_sizes");
                // Through JSON because Postgres arrays cannot nest ragged ones.
                let links: Vec<Option<Vec<String>>> =
                    serde_json::from_value(r.get("file_links")).unwrap_or_default();
                let files = paths
                    .into_iter()
                    .zip(hashes)
                    .zip(sizes)
                    .zip(links.into_iter().chain(std::iter::repeat(None)))
                    .map(|(((path, sha256), bytes), links)| SkillFile {
                        path,
                        sha256,
                        bytes,
                        links,
                    })
                    .collect();
                SkillVersion {
                    id: r.get("id"),
                    skill_id: r.get("skill_id"),
                    ordinal: r.get("ordinal"),
                    body: r.get("body"),
                    note: r.get("note"),
                    based_on_version_id: r.get("based_on_version_id"),
                    hosts: r.get("hosts"),
                    files,
                    unreached: None,
                    created_by: r.get("created_by"),
                    created_at: r.get("created_at"),
                }
                .with_unreached()
            })
            .collect())
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
        let mut version = row.as_ref().map(read_version).ok_or(SkillError::NotFound)?;
        version.hosts = hosts_of(&self.pool, version.id).await?;
        version.files = files_of(&self.pool, version.id).await?;
        Ok(version.with_unreached())
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

        let first = Uuid::now_v7();
        sqlx::query(
            "insert into skill_versions (id, workspace_id, skill_id, ordinal, body, note, created_by) \
             values ($1, $2, $3, 1, $4, $5, $6)",
        )
        .bind(first)
        .bind(workspace_id)
        .bind(id)
        .bind(&taken.body)
        .bind(format!("forked from {} v{}", source.name, taken.ordinal))
        .bind(author)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
        write_files(&mut tx, first, &taken.files).await?;
        // A fork copies the file list, so it copies what those files declare.
        copy_gates(&mut tx, taken.id, first).await?;

        tx.commit().await.map_err(internal)?;
        self.get(workspace_id, id).await
    }

    async fn record_links(&self, version_id: Uuid, files: &[SkillFile]) -> Result<(), SkillError> {
        let mut tx = self.pool.begin().await.map_err(internal)?;
        for f in files {
            sqlx::query(
                "update skill_version_files set links = $3 \
                 where version_id = $1 and path = $2 and links is null",
            )
            .bind(version_id)
            .bind(&f.path)
            .bind(&f.links)
            .execute(&mut *tx)
            .await
            .map_err(internal)?;
        }
        tx.commit().await.map_err(internal)
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
        // Bindings and turn history cascade from a skill, so a plain delete
        // would quietly take a skill away from agents using it and erase which
        // turns ran with it. Either is a reason to retire it instead.
        let done = sqlx::query(
            "delete from skills s where s.workspace_id = $1 and s.id = $2 \
             and not exists (select 1 from agent_skills a where a.skill_id = s.id) \
             and not exists (select 1 from turn_skills t where t.skill_id = s.id)",
        )
        .bind(workspace_id)
        .bind(id)
        .execute(&self.pool)
        .await
        .map_err(|e| map_write_error(e, ""))?;
        if done.rows_affected() == 0 {
            let exists: bool = sqlx::query_scalar(
                "select exists (select 1 from skills where workspace_id = $1 and id = $2)",
            )
            .bind(workspace_id)
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .map_err(internal)?;
            return Err(if exists {
                SkillError::Invalid(
                    "this skill is given to an agent or has run in a turn, so it is kept; \
                     retire it instead"
                        .into(),
                )
            } else {
                SkillError::NotFound
            });
        }
        Ok(())
    }

    async fn bindings(
        &self,
        workspace_id: Uuid,
        agent_id: Uuid,
    ) -> Result<Vec<Binding>, SkillError> {
        let rows = sqlx::query(
            "select skill_id, version_id, position from agent_skills \
             where workspace_id = $1 and agent_id = $2 \
             order by position",
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
            // From the skills to each one's newest version and then its
            // hosts: one probe of `skill_versions_current_idx` per skill.
            // Joined the other way it read the hosts of every version ever
            // written and kept the newest's.
            "select distinct h.host \
               from skills s \
               cross join lateral ( \
                   select id from skill_versions \
                    where skill_id = s.id order by ordinal desc limit 1) v \
               join skill_version_hosts h on h.version_id = v.id \
              where (s.id = any($2) \
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
                 select bd.position, 0 as tier, s.id as skill_id, s.slug, s.workspace_id as owner,
                        s.name, s.kind,
                        coalesce(bd.pinned, (select v.id from skill_versions v
                                              where v.skill_id = s.id
                                              order by v.ordinal desc limit 1)) as version_id
                   from bound bd
                   join skills s on s.id = bd.skill_id
                  where s.retired_at is null
                 union all
                 select bd.position, 1 as tier, o.id, o.slug, o.workspace_id, o.name, o.kind,
                        (select v.id from skill_versions v where v.skill_id = o.id
                          order by v.ordinal desc limit 1)
                   from bound bd
                   join skills o on o.base_skill_id = bd.skill_id
                  where o.workspace_id = $1 and o.kind = 'override' and o.retired_at is null
             )
             select p.position, p.tier, p.skill_id, p.version_id, p.slug, p.owner, p.name, p.kind,
                    v.body
               from picked p
               join skill_versions v on v.id = p.version_id
              order by p.position, p.tier",
        )
        .bind(workspace_id)
        .bind(agent_id)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        // Every version's files in one query rather than one per skill: this
        // runs before each turn starts.
        let versions: Vec<Uuid> = rows.iter().map(|r| r.get("version_id")).collect();
        let mut files: std::collections::HashMap<Uuid, Vec<SkillFile>> = Default::default();
        for f in sqlx::query(
            "select version_id, path, sha256, bytes from skill_version_files \
             where version_id = any($1) order by path",
        )
        .bind(&versions)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?
        {
            files
                .entry(f.get("version_id"))
                .or_default()
                .push(SkillFile {
                    path: f.get("path"),
                    sha256: f.get("sha256"),
                    bytes: f.get("bytes"),
                    links: None,
                });
        }

        Ok(rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let version_id: Uuid = r.get("version_id");
                ResolvedSkill {
                    skill_id: r.get("skill_id"),
                    version_id,
                    slug: r.get("slug"),
                    owner: r.get("owner"),
                    name: r.get("name"),
                    kind: SkillKind::parse(r.get::<String, _>("kind").as_str()),
                    body: r.get("body"),
                    files: files.get(&version_id).cloned().unwrap_or_default(),
                    position: i as i32,
                }
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
              where h.version_id = ( \
                  select id from skill_versions \
                   where skill_id = $2 order by ordinal desc limit 1) \
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
        if skills.is_empty() {
            return Ok(());
        }
        let reply_ids: Vec<Uuid> = vec![reply_id; skills.len()];
        let skill_ids: Vec<Uuid> = skills.iter().map(|s| s.skill_id).collect();
        let version_ids: Vec<Uuid> = skills.iter().map(|s| s.version_id).collect();
        let positions: Vec<i32> = skills.iter().map(|s| s.position).collect();
        sqlx::query(
            "insert into turn_skills (reply_id, skill_id, version_id, position) \
             select * from unnest($1::uuid[], $2::uuid[], $3::uuid[], $4::int[]) \
             on conflict do nothing",
        )
        .bind(&reply_ids)
        .bind(&skill_ids)
        .bind(&version_ids)
        .bind(&positions)
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(())
    }
}

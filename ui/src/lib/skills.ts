import { api, apiText, allPages } from './api'

/**
 * A skill is instructions an agent is given beside its system prompt.
 *
 * Two things own one: the operator, whose skills reach every workspace, and a
 * workspace itself. Which it is shows in `workspace_id`, so a page compares it
 * against the session's workspace rather than asking the server twice.
 */
export type SkillKind = 'standalone' | 'override'

export type Skill = {
  id: string
  workspace_id: string
  slug: string
  name: string
  description: string
  kind: SkillKind
  /** What an override layers onto. */
  base_skill_id: string | null
  forked_from_skill_id: string | null
  forked_from_version_id: string | null
  retired_at: string | null
  /** Hosts the live version says it will reach. Names them; opens nothing. */
  hosts: string[]
  /** Those this workspace has not allowed. Empty means ready to bind. */
  unmet_hosts: string[]
  /** The live version, which is always the newest. */
  version_id: string | null
  ordinal: number | null
  /** An override whose base has been edited since it was written against it.
   *  Nothing breaks -- which is why it has to be said out loud. */
  base_moved: boolean
}

/** A file as a version records it: no content, which is fetched separately. */
export type VersionFile = {
  path: string
  sha256: string
  bytes: number
  /** The other files of the version it names, recorded at publish. */
  links: string[] | null
}

export type SkillVersion = {
  id: string
  skill_id: string
  ordinal: number
  body: string
  note: string
  based_on_version_id: string | null
  hosts: string[]
  /** The files published with it. Part of the version as much as its prose is,
   *  so a reader deciding whether to restore one needs to see them. */
  files: VersionFile[]
  /** Files nothing leads an agent to, worked out by the API from the body and
   *  each file's links. Null when it is not known, which is not "none". */
  unreached: string[] | null
  created_by: string | null
  created_at: string
}

/** Which skills an agent is given. Overrides are never bound: they ride along
 *  with the base they speak about. */
export type Binding = {
  skill_id: string
  version_id: string | null
  position: number
}

export function listSkills(): Promise<Skill[]> {
  return allPages<Skill>('/v1/skills')
}

export function getSkill(id: string): Promise<Skill> {
  return api<Skill>(`/v1/skills/${id}`)
}

/** A version as the history lists it: enough to choose one, without its prose
 *  or file list. `getVersion` reads the rest. */
export type VersionSummary = {
  id: string
  skill_id: string
  ordinal: number
  note: string
  based_on_version_id: string | null
  hosts: string[]
  file_count: number
  /** What it did to the files of the version before it, by hash. */
  changed: FileChange[]
  unreached: string[] | null
  created_by: string | null
  created_at: string
}

export function listVersions(id: string): Promise<VersionSummary[]> {
  return allPages<VersionSummary>(`/v1/skills/${id}/versions`)
}

export function getVersion(id: string, versionId: string): Promise<SkillVersion> {
  return api<SkillVersion>(`/v1/skills/${id}/versions/${versionId}`)
}

export type NewSkill = {
  slug: string
  name: string
  description?: string
  body?: string
  /** Set to write an override of another skill rather than a skill of one's own. */
  base_skill_id?: string
  hosts?: string[]
  /** The first version's files. An override carries none. */
  files?: NewFile[]
}

/** A file as the API takes it. */
export type NewFile = { path: string; content: string }

/** A file as the editor holds it. */
export type DraftFile = {
  path: string
  /** Null until fetched: a version's files are listed at once and read in the
   *  background, so a skill of hundreds of them opens without waiting. */
  content: string | null
  bytes: number
  /** The other files it names, as the API recorded them. Null once that may no
   *  longer be true -- its text was edited, or a file was added or renamed that
   *  it might name -- after which they are worked out from the text here. */
  links: string[] | null
}

/** A file just written or uploaded here, so nothing about it came from the API. */
export function draftFile(path: string, content: string): DraftFile {
  return {
    path,
    content,
    bytes: new TextEncoder().encode(content).length,
    links: null,
  }
}

/**
 * Creating, editing and publishing all come in two flavours.
 *
 * `platform` sends the write to the operator's own routes, which land in the
 * platform workspace and are closed to everybody else. It is not a different
 * kind of skill, only a different owner.
 */
export function createSkill(input: NewSkill, platform = false): Promise<Skill> {
  return api<Skill>(platform ? '/v1/platform/skills' : '/v1/skills', {
    method: 'POST',
    body: JSON.stringify(input),
  })
}

export function updateSkill(
  id: string,
  input: { name?: string; description?: string },
  platform = false,
): Promise<Skill> {
  return api<Skill>(platform ? `/v1/platform/skills/${id}` : `/v1/skills/${id}`, {
    method: 'PATCH',
    body: JSON.stringify(input),
  })
}

/** Publishes a new version, which is the only way prose changes. A rollback
 *  comes through here too, carrying an old body forward. */
export function publishVersion(
  id: string,
  body: string,
  note: string,
  hosts: string[],
  platform = false,
  /** The whole set, when it changed. Left out, the new version keeps the
   *  previous one's files -- which is what an edit to the prose alone means. */
  files?: NewFile[],
): Promise<SkillVersion> {
  return api<SkillVersion>(
    platform ? `/v1/platform/skills/${id}/versions` : `/v1/skills/${id}/versions`,
    {
      method: 'POST',
      body: JSON.stringify({ body, note, hosts, ...(files ? { files } : {}) }),
    },
  )
}

/** One file of a version, as it was published. */
export function readVersionFile(id: string, versionId: string, path: string): Promise<string> {
  const encoded = path.split('/').map(encodeURIComponent).join('/')
  return apiText(`/v1/skills/${id}/versions/${versionId}/files/${encoded}`)
}

/** A version's files as the editor starts them: listed, with nothing read. */
export function versionDrafts(v: SkillVersion): DraftFile[] {
  return v.files
    .map((f) => ({
      path: f.path,
      content: null,
      bytes: f.bytes,
      links: f.links,
    }))
    .sort((a, b) => a.path.localeCompare(b.path))
}

/** Reads a version's files in the background. */
export type FileLoader = {
  /** Moves a file to the front, for the one somebody just opened. */
  first(path: string): void
  /** Every file read so far, by path. */
  contents: Map<string, string>
  /** Settles when every file is read, or the first read fails. */
  done: Promise<void>
  /** Stops reading and reports nothing further. */
  cancel(): void
}

/**
 * Reads every file of a version, a few at a time, reporting them in batches.
 *
 * In batches because each report re-renders the editor, and a report per file
 * would be one per file of a skill with hundreds.
 */
export function loadVersionFiles(
  v: SkillVersion,
  onLoaded: (batch: Map<string, string>) => void,
  width = 4,
): FileLoader {
  const queue = v.files.map((f) => f.path)
  const contents = new Map<string, string>()
  let pending = new Map<string, string>()
  let flush: ReturnType<typeof setTimeout> | null = null
  let cancelled = false

  function report() {
    flush = null
    if (cancelled || pending.size === 0) return
    const batch = pending
    pending = new Map()
    onLoaded(batch)
  }

  async function worker() {
    for (let path = queue.shift(); path !== undefined; path = queue.shift()) {
      if (cancelled) return
      const content = await readVersionFile(v.skill_id, v.id, path)
      contents.set(path, content)
      pending.set(path, content)
      flush ??= setTimeout(report, 50)
    }
  }

  const done = Promise.all(Array.from({ length: Math.min(width, queue.length) }, worker)).then(
    report,
  )
  return {
    first(path) {
      const i = queue.indexOf(path)
      if (i > 0) queue.unshift(...queue.splice(i, 1))
    },
    contents,
    done,
    cancel() {
      cancelled = true
      if (flush) clearTimeout(flush)
    },
  }
}

/** The most a file may hold: what an agent reads in one call. Mirrors the API. */
export const MAX_FILE_BYTES = 32 * 1024

/** Why a path would be refused, or null. The API's rules, checked as somebody
 *  types so they are not learned one publish at a time. */
export function pathProblem(path: string, others: string[]): string | null {
  if (path.length === 0) return 'needs a name'
  if (new TextEncoder().encode(path).length > 256) return 'is too long'
  if (path.startsWith('/')) return 'must not start with /'
  // eslint-disable-next-line no-control-regex
  if (/[\u0000-\u001f\u007f\\]/.test(path)) return 'contains a backslash or a control character'
  if (path.split('/').some((seg) => seg === '' || seg === '.' || seg === '..')) {
    return 'has an empty, . or .. part'
  }
  if (others.includes(path)) return 'is already a file here'
  return null
}

/**
 * Whether `text` names the file at `path`, either as the full path an agent
 * reads (`skill/<slug>/<path>`) or bare.
 *
 * The same rule as `files::mentions` in the API, which records what each file
 * names at publish; the two must agree, or a file the editor calls reachable
 * is one the API calls unreached.
 */
export function mentions(text: string, path: string): boolean {
  const name = (c: string | undefined) => c !== undefined && /[\p{L}\p{N}_-]/u.test(c)
  for (let i = text.indexOf(path); i !== -1; i = text.indexOf(path, i + 1)) {
    const before = text[i - 1]
    // A full stop may end the sentence the name is in; it may not begin it.
    const after = text[i + path.length]
    if (!(name(before) || before === '.') && !(name(after) || after === '/')) return true
  }
  return false
}

/** What each file names, worked out here, kept until its text or the set of
 *  paths changes: an edit makes a new object, so only that file is redone. */
const worked = new WeakMap<DraftFile, { paths: string; links: string[] }>()

function linksOf(f: DraftFile, paths: string[], key: string): string[] | null {
  if (f.links !== null) return f.links
  if (f.content === null) return null
  const known = worked.get(f)
  if (known?.paths === key) return known.links
  const content = f.content
  const links = paths.filter((p) => p !== f.path && mentions(content, p))
  worked.set(f, { paths: key, links })
  return links
}

/**
 * The files an agent can find by following names from the main instructions,
 * through files that name other files -- a wizard's skill names its category
 * files, and each of those names its operations' own.
 *
 * `complete` is false while a reached file whose links are unknown is still
 * being read: until then a file not yet reached may yet be, so none is reported.
 */
export function reachable(
  body: string,
  files: DraftFile[],
): { reached: Set<string>; complete: boolean } {
  const paths = files.map((f) => f.path)
  const key = paths.join('\n')
  const byPath = new Map(files.map((f) => [f.path, f]))
  const reached = new Set(paths.filter((p) => mentions(body, p)))
  const queue = [...reached]
  let complete = true
  for (let path = queue.pop(); path !== undefined; path = queue.pop()) {
    const links = linksOf(byPath.get(path)!, paths, key)
    if (links === null) {
      complete = false
      continue
    }
    for (const next of links) {
      if (byPath.has(next) && !reached.has(next)) {
        reached.add(next)
        queue.push(next)
      }
    }
  }
  return { reached, complete }
}

/** Whether a file declares that the operation it documents needs approving. */
export function declaresApproval(content: string): boolean {
  const m = /^---\r?\n([\s\S]*?)\r?\n---/.exec(content)
  return m !== null && /^approval\s*:/m.test(m[1])
}

/**
 * Opens the hosts this skill names that the workspace has not allowed.
 *
 * Needs the authority that writes an egress rule, which is deliberately not the
 * one that writes skills: declaring a host asks for access, it does not take it.
 */
export function approveHosts(id: string): Promise<string[]> {
  return api<string[]>(`/v1/skills/${id}/hosts/approve`, { method: 'POST' })
}

/** One host per line, as the editor shows them. */
export function parseHosts(text: string): string[] {
  return text
    .split(/[\n,]/)
    .map((h) => h.trim())
    .filter(Boolean)
}

export function retireSkill(id: string, retired: boolean, platform = false): Promise<Skill> {
  return api<Skill>(platform ? `/v1/platform/skills/${id}/retired` : `/v1/skills/${id}/retired`, {
    method: 'PUT',
    body: JSON.stringify({ retired }),
  })
}

/** Takes a copy, keeping only a note of where it came from. Nothing merges back. */
export function forkSkill(
  id: string,
  input: { slug: string; name: string; version_id?: string },
): Promise<Skill> {
  return api<Skill>(`/v1/skills/${id}/fork`, {
    method: 'POST',
    body: JSON.stringify(input),
  })
}

export function deleteSkill(id: string): Promise<void> {
  return api<void>(`/v1/skills/${id}`, { method: 'DELETE' })
}

export function agentSkills(agentId: string): Promise<Binding[]> {
  return api<Binding[]>(`/v1/agents/${agentId}/skills`)
}

export function setAgentSkills(agentId: string, bindings: Binding[]): Promise<Binding[]> {
  return api<Binding[]>(`/v1/agents/${agentId}/skills`, {
    method: 'PUT',
    body: JSON.stringify(bindings),
  })
}

/** A slug the server will accept, derived from a name as it is typed. */
export function slugify(name: string): string {
  return name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 60)
}

/** How one file differs between two versions. */
export type FileChange = { path: string; change: 'added' | 'removed' | 'changed' }

type Carried = { path: string; sha256: string }

/**
 * Which files differ between two versions, by path, going from `from` to `to`.
 *
 * Compared by hash, which every version already records, so a history of many
 * versions can say what each one changed without reading a byte of any file.
 * Files that are the same are left out: the question is what changed.
 */
export function fileChanges(from: Carried[], to: Carried[]): FileChange[] {
  const before = new Map(from.map((f) => [f.path, f.sha256]))
  const after = new Map(to.map((f) => [f.path, f.sha256]))
  const out: FileChange[] = []
  for (const [path, hash] of after) {
    const was = before.get(path)
    if (was === undefined) out.push({ path, change: 'added' })
    else if (was !== hash) out.push({ path, change: 'changed' })
  }
  for (const path of before.keys()) {
    if (!after.has(path)) out.push({ path, change: 'removed' })
  }
  return out.sort((a, b) => a.path.localeCompare(b.path))
}

/** What a specification answers of the import form, read before anything is
 *  created. Every field is a suggestion; `base_url` and `auth_header` are null
 *  when the document did not say. */
export type OpenApiPreview = {
  name: string
  description: string
  slug: string
  base_url: string | null
  auth_header: string | null
  /** What `auth_header` serves, when a declared scheme chose it. */
  auth_kind: AuthKind | null
  /** Operations needing a credential the header does not carry, and what
   *  they need instead. Those will not authenticate on their own. */
  unserved_operations: number
  unserved_kinds: AuthKind[]
  credential_env: string
  operations: number
  categories: number
}

/** A security scheme as an egress rule sees it. A rule attaches one static
 *  header, so the first three are servable and the rest are not. */
export type AuthKind =
  | 'api_key_header'
  | 'bearer'
  | 'basic'
  | 'api_key_query'
  | 'api_key_cookie'
  | 'oauth2'
  | 'open_id_connect'
  | 'mutual_tls'
  | 'other'

export const authKindLabel: Record<AuthKind, string> = {
  api_key_header: 'an API key in a header',
  bearer: 'a bearer token',
  basic: 'basic credentials',
  api_key_query: 'an API key in the query string',
  api_key_cookie: 'an API key in a cookie',
  oauth2: 'OAuth 2.0',
  open_id_connect: 'OpenID Connect',
  mutual_tls: 'mutual TLS',
  other: 'a scheme the platform does not recognise',
}

/** The specification travels parsed, as the object it is, to both routes --
 *  or as a URL the server fetches through the gateway, in which case the
 *  preview hands back what it fetched as `spec`, for the create to send. */
export function previewOpenApi(
  source: { spec: unknown } | { url: string },
): Promise<OpenApiPreview & { spec?: unknown }> {
  return api<OpenApiPreview & { spec?: unknown }>('/v1/platform/skills/from-openapi/preview', {
    method: 'POST',
    body: JSON.stringify(source),
  })
}

export function createFromOpenApi(input: {
  slug: string
  name: string
  description: string
  base_url: string
  /** The header the egress rule will carry the credential in. The skill
   *  tells the agent the platform sets it, and drops it from every
   *  operation's parameters. Empty when the API takes no credential. */
  auth_header: string
  spec: unknown
}): Promise<Skill> {
  return api<Skill>('/v1/platform/skills/from-openapi', {
    method: 'POST',
    body: JSON.stringify(input),
  })
}

/** Something a person added to a generated skill, at the level it is seen. */
export type Annotation = {
  id: string
  level: 'skill' | 'category' | 'operation'
  target: string | null
  kind: 'note' | 'prefer' | 'hidden' | 'approval'
  value: string
  created_at: string
}

export type SkillSource = {
  base_url: string
  auth_header: string | null
  revision: { id: string; spec_sha256: string; spec_bytes: number; created_at: string } | null
  annotations: Annotation[]
  outline: {
    categories: string[]
    operations: { name: string; category: string; method: string; path: string }[]
  } | null
}

export type Regenerated = {
  revision: string
  body_changed: boolean
  changed: FileChange[]
  unmatched: Annotation[]
  /** Gates the live version has that this would not, as `METHOD path`.
   *  Publishing refuses unless `remove_gates` says to. */
  lost_gates: string[]
  version: VersionSummary | null
}

/** What a generated skill is made from; null for one written by hand. */
export async function getSource(id: string): Promise<SkillSource | null> {
  try {
    return await api<SkillSource>(`/v1/skills/${id}/source`)
  } catch (e) {
    if (e instanceof Error && 'status' in e && (e as { status: number }).status === 404) return null
    throw e
  }
}

export const annotate = (id: string, a: Pick<Annotation, 'level' | 'target' | 'kind' | 'value'>) =>
  api<Annotation>(`/v1/platform/skills/${id}/annotations`, {
    method: 'POST',
    body: JSON.stringify(a),
  })

export const retireAnnotation = (id: string, annotation: string) =>
  api<void>(`/v1/platform/skills/${id}/annotations/${annotation}`, { method: 'DELETE' })

/** Generates the skill again: a proposal, unless `publish`. `spec` keeps a new
 *  specification as the next revision. */
export const regenerate = (
  id: string,
  input: { publish?: boolean; remove_gates?: boolean; spec?: string; note?: string },
) =>
  api<Regenerated>(`/v1/platform/skills/${id}/regenerate`, {
    method: 'POST',
    body: JSON.stringify(input),
  })

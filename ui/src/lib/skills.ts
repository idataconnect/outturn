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
  files: { path: string; sha256: string; bytes: number }[]
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

export function listVersions(id: string): Promise<SkillVersion[]> {
  return allPages<SkillVersion>(`/v1/skills/${id}/versions`)
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
  files?: DraftFile[]
}

/** A file as written in the editor: a path under the skill, and its text. */
export type DraftFile = { path: string; content: string }

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
  files?: DraftFile[],
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

/** Every file a version carries, with its text, in path order. */
export async function readVersionFiles(v: SkillVersion): Promise<DraftFile[]> {
  const files = await Promise.all(
    v.files.map(async (f) => ({
      path: f.path,
      content: await readVersionFile(v.skill_id, v.id, f.path),
    })),
  )
  return files.sort((a, b) => a.path.localeCompare(b.path))
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
 * Whether the main instructions mention a file at all.
 *
 * Files are never sent to the agent; it reads one only when the instructions
 * tell it to. A file they never name is one the agent will not know exists, so
 * it is flagged rather than left to be discovered by nobody. Either the full
 * path the agent reads (`skill/<slug>/<path>`) or the bare path counts.
 */
export function mentionedIn(body: string, slug: string, path: string): boolean {
  return body.includes(`skill/${slug}/${path}`) || body.includes(path)
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

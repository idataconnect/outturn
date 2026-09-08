import { api } from './api'

/**
 * A skill is prose an agent is given beside its system prompt.
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
  return api<Skill[]>('/v1/skills')
}

export function getSkill(id: string): Promise<Skill> {
  return api<Skill>(`/v1/skills/${id}`)
}

export function listVersions(id: string): Promise<SkillVersion[]> {
  return api<SkillVersion[]>(`/v1/skills/${id}/versions`)
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
  platform = false,
): Promise<SkillVersion> {
  return api<SkillVersion>(
    platform ? `/v1/platform/skills/${id}/versions` : `/v1/skills/${id}/versions`,
    { method: 'POST', body: JSON.stringify({ body, note }) },
  )
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

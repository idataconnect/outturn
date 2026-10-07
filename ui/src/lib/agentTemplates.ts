import { allPages, api } from './api'

/**
 * An agent the operator defines once and has made in every workspace that
 * should have it. See docs/agent-templates.md.
 */
export type Availability = 'required' | 'default' | 'optional'

export type TemplateSkill = { skill_id: string; version_id?: string | null }

export type TemplateVersion = {
  id: string
  template_id: string
  ordinal: number
  name: string
  description: string
  requirements: string
  defaults: string
  reminder: string
  policy: Record<string, unknown>
  eager_tools: string[]
  skills: TemplateSkill[]
  /** Settings it fixes, by catalog key. They win over the workspace's and
   *  the agent's own. */
  settings: Record<string, unknown>
  note: string
  created_at: string
}

export type Template = {
  id: string
  slug: string
  availability: Availability
  allow_additions: boolean
  /** Whether a workspace may keep its agent on a version. */
  allow_pinning: boolean
  retired: boolean
  /** The newest version, which every agent made from it follows. */
  current: TemplateVersion
}

/** What a publish says: the whole of a version, since versions are not edited. */
export type NewVersion = {
  name: string
  description: string
  requirements: string
  defaults: string
  reminder: string
  policy: Record<string, unknown>
  eager_tools: string[]
  skills: TemplateSkill[]
  settings: Record<string, unknown>
  note: string
}

/** A template as a workspace's catalog shows it. */
export type CatalogEntry = {
  template_id: string
  slug: string
  name: string
  description: string
  availability: Availability
  /** This workspace's agent made from it, if it has one. */
  agent_id: string | null
}

/** How each availability is described to an operator choosing it. */
export const AVAILABILITY: Record<Availability, { label: string; detail: string }> = {
  required: {
    label: 'Required',
    detail: 'Made in every workspace. A workspace cannot remove it.',
  },
  default: {
    label: 'Default',
    detail: 'Made in every workspace. A workspace may remove it.',
  },
  optional: {
    label: 'Optional',
    detail: 'Offered to every workspace, and made when one adds it.',
  },
}

export const listTemplates = () => allPages<Template>('/v1/platform/agent-templates')

export const getTemplate = (id: string) => api<Template>(`/v1/platform/agent-templates/${id}`)

export const createTemplate = (
  input: NewVersion & {
    slug: string
    availability: Availability
    allow_additions: boolean
    allow_pinning: boolean
  },
) =>
  api<Template>('/v1/platform/agent-templates', {
    method: 'POST',
    body: JSON.stringify(input),
  })

export const publishTemplate = (id: string, input: NewVersion) =>
  api<Template>(`/v1/platform/agent-templates/${id}/versions`, {
    method: 'POST',
    body: JSON.stringify(input),
  })

export const updateTemplate = (
  id: string,
  input: Partial<{
    availability: Availability
    allow_additions: boolean
    allow_pinning: boolean
    retired: boolean
  }>,
) =>
  api<Template>(`/v1/platform/agent-templates/${id}`, {
    method: 'PATCH',
    body: JSON.stringify(input),
  })

export const templateCatalog = () => allPages<CatalogEntry>('/v1/agent-templates')

export const installTemplate = (id: string) =>
  api<{ agent_id: string }>(`/v1/agent-templates/${id}/install`, { method: 'POST' })

/** One version of an agent's template, as its workspace chooses between them. */
export type VersionChoice = { id: string; ordinal: number; note: string; created_at: string }

export const agentTemplateVersions = (agentId: string) =>
  allPages<VersionChoice>(`/v1/agents/${agentId}/template-versions`)

/** Keeps an agent on a version, or with null follows the newest again. */
export const pinTemplateVersion = (agentId: string, versionId: string | null) =>
  api<void>(`/v1/agents/${agentId}/template-version`, {
    method: 'PUT',
    body: JSON.stringify({ version_id: versionId }),
  })

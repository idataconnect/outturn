/**
 * Where the administrative pages live.
 *
 * Settings is the workspace's own administration, reached rarely and together:
 * its settings, who is in it, and what roles allow. Platform is what crosses
 * every workspace and belongs to the operator alone. They are separate so the
 * scope of a page is never in doubt -- "Roles" beside "Workspaces" read as
 * though both were this workspace's.
 *
 * Written once here so that moving a page is one edit, not twenty strings.
 */
export const paths = {
  inbox: '/inbox',
  inboxItem: (id: string) => `/inbox/${id}`,

  settings: '/settings',
  users: '/settings/users',
  newUser: '/settings/users/new',
  user: (id: string) => `/settings/users/${id}`,
  roles: '/settings/roles',
  connections: '/settings/connections',
  newRole: '/settings/roles/new',
  role: (id: string) => `/settings/roles/${id}`,

  platform: '/platform',
  workspaces: '/platform/workspaces',
  newWorkspace: '/platform/workspaces/new',
  workspace: (id: string) => `/platform/workspaces/${id}`,
  platformDefaults: '/platform/defaults',
  agentTemplates: '/platform/agent-templates',
  agentTemplateNew: '/platform/agent-templates/new',
  agentTemplate: (id: string) => `/platform/agent-templates/${id}`,
}

import SettingsCascade from '../components/SettingsCascade'
import { useSession } from '../lib/session'

/** How agents in this workspace behave: the workspace's level of the cascade. */
export default function WorkspaceSettings() {
  const state = useSession()
  const authorities = state.status === 'authenticated' ? state.session.authorities : []
  const canEdit = authorities.includes('settings:update')

  return (
    <div className="p-6 max-w-3xl space-y-6">
      <div>
        <h1 className="text-2xl font-semibold text-surface-900 dark:text-surface-100">
          Workspace
        </h1>
        <p className="mt-2 text-surface-600 dark:text-surface-400">
          How agents in this workspace behave. Each value comes from the platform unless
          overridden here, and an agent can override again on its own page.
        </p>
      </div>
      <SettingsCascade base="/v1/settings" canEdit={canEdit} levelName="this workspace" />
    </div>
  )
}

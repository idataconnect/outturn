import SettingsCascade from '../components/SettingsCascade'

/**
 * What every workspace gets unless it overrides: the operator's level of the
 * cascade. Under Platform rather than a tab of a workspace's settings, because
 * it is not that workspace's, and a tab beside "This workspace" said it was.
 */
export default function PlatformDefaults() {
  return (
    <div className="p-6 max-w-3xl space-y-6">
      <div>
        <h1 className="text-2xl font-semibold text-surface-900 dark:text-surface-100">
          Defaults
        </h1>
        <p className="mt-2 text-surface-600 dark:text-surface-400">
          What every workspace gets unless it overrides. Visible to the operator only.
        </p>
      </div>
      <SettingsCascade base="/v1/platform/settings" canEdit levelName="the platform" />
    </div>
  )
}

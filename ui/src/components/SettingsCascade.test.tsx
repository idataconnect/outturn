import { render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import SettingsCascade, { type Effective } from './SettingsCascade'

// A ceiling whose built-in default is its strictest choice, as the
// auto-approval ceiling's is.
const ceiling: Effective = {
  key: 'auto_approve_max_risk',
  label: 'Auto-approval',
  description: 'The highest risk an auto-approval policy may cover.',
  kind: {
    type: 'choice',
    options: [
      { value: 'none', label: 'None' },
      { value: 'high', label: 'Any risk' },
    ],
  },
  default: 'none',
  owner: 'operator_ceiling',
  value: 'none',
  source: 'default',
  override_value: undefined,
  inherited: 'none',
  per_model: false,
}

vi.mock('../lib/api', () => ({
  ApiError: class extends Error {},
  api: vi.fn(async (path: string) => (path.startsWith('/v1/models') ? [] : [ceiling])),
}))

describe('SettingsCascade', () => {
  it('leaves a workspace nothing to choose under a ceiling at its strictest', async () => {
    render(<SettingsCascade base="/v1/settings" canEdit levelName="this workspace" />)
    const override = (await screen.findByRole('checkbox')) as HTMLInputElement
    expect(override.disabled).toBe(true)
  })

  it('lets the operator raise a ceiling that starts at its strictest', async () => {
    render(
      <SettingsCascade
        base="/v1/platform/settings"
        canEdit
        levelName="the platform"
        setsCeilings
      />,
    )
    const override = (await screen.findByRole('checkbox')) as HTMLInputElement
    expect(override.disabled).toBe(false)
  })
})

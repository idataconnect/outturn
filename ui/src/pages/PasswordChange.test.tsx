import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import { PasswordChange } from './UserEditor'

const sent: { path: string; body: unknown }[] = []

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api')
  return {
    ...actual,
    api: vi.fn(async (path: string, init?: RequestInit) => {
      sent.push({ path, body: JSON.parse(init!.body as string) })
      return undefined
    }),
  }
})

describe('PasswordChange', () => {
  beforeEach(() => {
    sent.length = 0
  })

  it('asks for the current password when the account is your own', async () => {
    render(<PasswordChange userId="u1" identityId="i1" own />)
    fireEvent.click(screen.getByRole('button', { name: 'Change password' }))
    fireEvent.change(screen.getByLabelText('Current password'), { target: { value: 'old-horse' } })
    fireEvent.change(screen.getByLabelText('New password'), { target: { value: 'battery-staple' } })
    fireEvent.click(screen.getByRole('button', { name: 'Save password' }))
    await waitFor(() => expect(screen.getByRole('status')).toHaveTextContent('Password changed.'))
    expect(sent).toEqual([
      {
        path: '/v1/users/u1/identities/i1/password',
        body: { current_password: 'old-horse', new_password: 'battery-staple' },
      },
    ])
  })

  it('sets one without the current password for somebody else', async () => {
    render(<PasswordChange userId="u2" identityId="i2" own={false} />)
    fireEvent.click(screen.getByRole('button', { name: 'Change password' }))
    expect(screen.queryByLabelText('Current password')).not.toBeInTheDocument()
    fireEvent.change(screen.getByLabelText('New password'), { target: { value: 'battery-staple' } })
    fireEvent.click(screen.getByRole('button', { name: 'Save password' }))
    await waitFor(() => expect(sent).toHaveLength(1))
    expect(sent[0].body).toEqual({ new_password: 'battery-staple' })
  })
})

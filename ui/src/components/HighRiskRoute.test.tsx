import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import HighRiskRoute from './HighRiskRoute'
import { getRoutes, putHighRoute } from '../lib/autoApproval'

vi.mock('../lib/autoApproval', async (actual) => ({
  ...(await actual<typeof import('../lib/autoApproval')>()),
  getRoutes: vi.fn(),
  putHighRoute: vi.fn(),
}))

const get = vi.mocked(getRoutes)
const put = vi.mocked(putHighRoute)

const answerers = [
  { role_id: 'r-admin', name: 'admin', members: ['Alice', 'Bob'] },
  { role_id: 'r-manager', name: 'manager', members: [] },
]

describe('HighRiskRoute', () => {
  beforeEach(() => {
    get.mockReset()
    put.mockReset()
  })

  it('names the roles asked, and sends the ones chosen', async () => {
    get.mockResolvedValue({ high: [], answerers, may_write: true })
    put.mockResolvedValue({ high: [answerers[1]], answerers, may_write: true })
    render(<HighRiskRoute />)
    const manager = await screen.findByLabelText(/The manager role/)
    expect(screen.getByRole('button', { name: 'Save' })).toHaveProperty('disabled', true)
    await userEvent.click(manager)
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))
    expect(put).toHaveBeenCalledWith(['r-manager'])
    expect(await screen.findByRole('status')).toHaveTextContent(/Saved/)
  })

  it('shows who is chosen already, and changes nothing for somebody who may not', async () => {
    get.mockResolvedValue({ high: [answerers[0]], answerers, may_write: false })
    render(<HighRiskRoute />)
    expect(await screen.findByLabelText(/The admin role/)).toHaveProperty('checked', true)
    expect(screen.getByLabelText(/The admin role/)).toBeDisabled()
    expect(screen.getByText('Alice, Bob')).toBeInTheDocument()
    expect(screen.getByText('Nobody holds it here.')).toBeInTheDocument()
    expect(screen.queryByRole('button')).toBeNull()
  })
})

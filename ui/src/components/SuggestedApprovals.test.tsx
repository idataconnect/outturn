import { fireEvent, render, screen, within } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import SuggestedApprovals from './SuggestedApprovals'
import type { Annotation, SkillSource } from '../lib/skills'

type Operation = NonNullable<SkillSource['outline']>['operations'][number]

const charge: Operation = {
  name: 'create_charge',
  category: 'Charges',
  method: 'POST',
  path: '/charges',
  suggested_approval: {
    risk: 'high',
    gate: true,
    requires: 'charge',
    matches_path: '/charges',
    binds: ['amount_pence', 'customer_id'],
    numbers: ['amount_pence'],
    amount: { field: 'amount_pence', currency: 'GBP', minor_units: 2 },
    reason: 'named for charge, which moves money or cannot be undone',
  },
}
const list: Operation = {
  name: 'list_charges',
  category: 'Charges',
  method: 'GET',
  path: '/charges',
  suggested_approval: {
    risk: 'low',
    gate: false,
    requires: 'list_charges',
    matches_path: '/charges',
    binds: [],
    numbers: [],
    reason: 'reads change nothing',
  },
}
const remove: Operation = {
  name: 'delete_customer',
  category: 'Customers',
  method: 'DELETE',
  path: '/customers/{id}',
  suggested_approval: {
    risk: 'high',
    gate: true,
    requires: 'delete',
    matches_path: '/customers/{id}',
    binds: ['id'],
    numbers: [],
    reason: 'a delete cannot be taken back',
  },
}
const ping: Operation = {
  name: 'send_ping',
  category: 'Health',
  method: 'POST',
  path: '/ping',
  suggested_approval: {
    risk: 'high',
    gate: true,
    requires: 'ping',
    matches_path: '/ping',
    binds: [],
    numbers: [],
    reason: 'the specification declares x-risk',
  },
}

function setup(annotations: Annotation[] = []) {
  const onConfirm = vi.fn()
  render(
    <SuggestedApprovals
      operations={[charge, list, remove, ping]}
      annotations={annotations}
      baseUrl="https://api.example.com/v1"
      busy={false}
      onConfirm={onConfirm}
    />,
  )
  return onConfirm
}

describe('suggested approvals', () => {
  it('shows each operation with its risk, whether it is gated, and its amount', () => {
    setup()
    const row = within(screen.getByTestId('suggestion-create_charge'))
    expect(row.getByText('high')).toBeInTheDocument()
    expect(row.getByText('Gated')).toBeInTheDocument()
    expect(row.getByText(/amount_pence · GBP/)).toBeInTheDocument()
    const read = within(screen.getByTestId('suggestion-list_charges'))
    expect(read.getByText('low')).toBeInTheDocument()
    expect(read.getByText('Not gated')).toBeInTheDocument()
  })

  it('writes nothing until a suggestion is confirmed', () => {
    const onConfirm = setup()
    expect(onConfirm).not.toHaveBeenCalled()
  })

  it('confirms one operation as the approval rule it suggested', () => {
    const onConfirm = setup()
    fireEvent.click(screen.getByRole('button', { name: 'Confirm create_charge' }))
    expect(onConfirm).toHaveBeenCalledTimes(1)
    const [[rules]] = onConfirm.mock.calls
    expect(rules).toEqual([
      {
        target: 'create_charge',
        value: [
          'approval:',
          '  requires: charge',
          '  matches: POST /v1/charges',
          '  binds: [amount_pence, customer_id]',
          '  risk: high',
          '  numbers: [amount_pence]',
          '  amount: { field: amount_pence, currency: GBP, minor_units: 2 }',
        ].join('\n'),
      },
    ])
  })

  it('cannot confirm what is not gated or has nothing for a rule to be about', () => {
    setup()
    expect(screen.getByRole('button', { name: 'Confirm list_charges' })).toBeDisabled()
    expect(screen.getByRole('button', { name: 'Confirm send_ping' })).toBeDisabled()
    expect(screen.getAllByText(/cannot be written/).length).toBeGreaterThan(0)
  })

  it('confirms a delete by path as a rule binding its id', () => {
    const onConfirm = setup()
    fireEvent.click(screen.getByRole('button', { name: 'Confirm delete_customer' }))
    const [[rules]] = onConfirm.mock.calls
    expect(rules[0].value).toContain('  matches: DELETE /v1/customers/{id}')
    expect(rules[0].value).toContain('  binds: [id]')
  })

  it('offers no path parameter as the amount', () => {
    setup()
    fireEvent.click(screen.getByRole('button', { name: 'Change delete_customer' }))
    const options = within(screen.getByLabelText('Amount field')).getAllByRole('option')
    expect(options.map((o) => o.textContent)).toEqual(['No amount'])
  })

  it('accepts every suggestion that is gated and can have a rule', () => {
    const onConfirm = setup()
    fireEvent.click(screen.getByRole('button', { name: /Accept all suggested \(2\)/ }))
    const [[rules]] = onConfirm.mock.calls
    expect(rules.map((r: { target: string }) => r.target)).toEqual([
      'create_charge',
      'delete_customer',
    ])
  })

  it('leaves an operation that already has a rule alone', () => {
    const onConfirm = setup([
      {
        id: 'a',
        level: 'operation',
        target: 'create_charge',
        kind: 'approval',
        value: 'approval: ...',
        created_at: '',
      },
    ])
    // Only the delete is left for a bulk accept.
    expect(screen.getByRole('button', { name: /Accept all suggested \(1\)/ })).toBeEnabled()
    expect(screen.queryByRole('button', { name: 'Confirm create_charge' })).not.toBeInTheDocument()
    expect(onConfirm).not.toHaveBeenCalled()
  })

  it('writes the override the operator chose instead of the guess', () => {
    const onConfirm = setup()
    fireEvent.click(screen.getByRole('button', { name: 'Change create_charge' }))
    fireEvent.change(screen.getByLabelText('Risk'), { target: { value: 'medium' } })
    fireEvent.change(screen.getByLabelText('Currency'), { target: { value: 'usd' } })
    fireEvent.click(screen.getByRole('button', { name: 'Confirm as changed' }))
    const [[rules]] = onConfirm.mock.calls
    expect(rules[0].value).toContain('risk: medium')
    expect(rules[0].value).toContain('amount: { field: amount_pence, currency: USD, minor_units: 2 }')
  })

  it('writes no amount when the operator takes it away', () => {
    const onConfirm = setup()
    fireEvent.click(screen.getByRole('button', { name: 'Change create_charge' }))
    fireEvent.change(screen.getByLabelText('Amount field'), { target: { value: '' } })
    fireEvent.click(screen.getByRole('button', { name: 'Confirm as changed' }))
    const [[rules]] = onConfirm.mock.calls
    expect(rules[0].value).not.toContain('amount:')
  })

  it('lets a read the operator wants gated be gated', () => {
    const onConfirm = vi.fn()
    render(
      <SuggestedApprovals
        operations={[
          {
            ...list,
            suggested_approval: { ...list.suggested_approval!, binds: ['q'] },
          },
        ]}
        annotations={[]}
        baseUrl={null}
        busy={false}
        onConfirm={onConfirm}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: 'Change list_charges' }))
    fireEvent.click(screen.getByLabelText('Needs approval'))
    fireEvent.click(screen.getByRole('button', { name: 'Confirm as changed' }))
    expect(onConfirm.mock.calls[0][0][0].value).toContain('matches: GET /charges')
  })
})

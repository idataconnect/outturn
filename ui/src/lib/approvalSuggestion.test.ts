import { describe, expect, it } from 'vitest'

import { approvalYaml, choiceFrom } from './approvalSuggestion'
import type { SuggestedApproval } from './skills'

const suggested: SuggestedApproval = {
  risk: 'high',
  gate: true,
  requires: 'charge',
  matches_path: '/charges',
  binds: ['payment_account_id', 'booking_id', 'amount_pence'],
  numbers: ['amount_pence'],
  amount: { field: 'amount_pence', currency: 'GBP', minor_units: 2 },
  reason: 'named for charge',
}
const op = { name: 'create_charge', category: 'Charges', method: 'POST', path: '/charges' }

/// The amount key as docs/auto-approval.md writes it ("Who sets what"): a flow
/// mapping with these three keys, in this order.
describe('the approval rule the page writes', () => {
  it('writes the amount as the design document shows it', () => {
    const yaml = approvalYaml(
      { ...op, suggested_approval: suggested },
      choiceFrom(suggested),
      'https://api.example.com',
    )
    expect(yaml.split('\n')).toContain(
      '  amount: { field: amount_pence, currency: GBP, minor_units: 2 }',
    )
    expect(yaml.split('\n').slice(0, 5)).toEqual([
      'approval:',
      '  requires: charge',
      '  matches: POST /charges',
      '  binds: [payment_account_id, booking_id, amount_pence]',
      '  risk: high',
    ])
  })

  it('leaves out the currency and units that were not given', () => {
    const bare = { ...suggested, amount: { field: 'amount_pence' } }
    const yaml = approvalYaml(
      { ...op, suggested_approval: bare },
      choiceFrom(bare),
      null,
    )
    expect(yaml).toContain('  amount: { field: amount_pence }')
  })
})

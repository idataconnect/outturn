import { describe, expect, it } from 'vitest'

import { amountComplete, amountFields, approvalYaml, cannotWrite, choiceFrom } from './approvalSuggestion'
import type { SuggestedApproval } from './skills'

const suggested: SuggestedApproval = {
  risk: 'high',
  gate: true,
  requires: 'charge',
  matches_path: '/charges',
  path_params: [],
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

  it('leaves out an amount whose currency or units are not known', () => {
    // The parser refuses an amount missing either, so writing it would make a
    // rule that cannot be published -- which accepting every suggestion did.
    const bare = { ...suggested, amount: { field: 'amount_pence' } }
    const yaml = approvalYaml(
      { ...op, suggested_approval: bare },
      choiceFrom(bare),
      null,
    )
    expect(yaml).not.toContain('amount:')
    expect(yaml).toContain('  risk: ')
  })

  it('binds and writes the field a request names its currency in', () => {
    const named = {
      ...suggested,
      amount: { field: 'amount_pence', currency: 'GBP', minor_units: 2, currency_field: 'currency' },
    }
    const yaml = approvalYaml({ ...op, suggested_approval: named }, choiceFrom(named), null)
    expect(yaml.split('\n')).toContain(
      '  amount: { field: amount_pence, currency: GBP, minor_units: 2, currency_field: currency }',
    )
    expect(yaml).toContain('  binds: [payment_account_id, booking_id, amount_pence, currency]')
  })

  /// The same text `crates/api/src/skill/wizard/tests.rs` publishes beside the
  /// operation the wizard generates (DELETE_INVOICE_RULE), so what the page
  /// writes for a delete by path is what the parser reads.
  it('writes a delete by path as a rule binding its id', () => {
    const remove: SuggestedApproval = {
      risk: 'high',
      gate: true,
      requires: 'delete',
      matches_path: '/invoices/{id}',
      path_params: ['id'],
      binds: ['id'],
      numbers: [],
      reason: 'a delete cannot be taken back',
    }
    const yaml = approvalYaml(
      {
        name: 'delete_invoice',
        category: 'Invoices',
        method: 'DELETE',
        path: '/invoices/{id}',
        suggested_approval: remove,
      },
      choiceFrom(remove),
      'https://books.example.com/v1',
    )
    expect(yaml).toBe(
      [
        'approval:',
        '  requires: delete',
        '  matches: DELETE /v1/invoices/{id}',
        '  binds: [id]',
        '  risk: high',
        '  numbers: []',
      ].join('\n'),
    )
    expect(cannotWrite(remove)).toBeNull()
    expect(amountFields(remove)).toEqual([])
  })

  it('counts an amount complete only with its currency and units', () => {
    const base = { gate: true, risk: 'high' as const, amount: 'amount', currency: '', minorUnits: '', currencyField: '' }
    expect(amountComplete(base)).toBe(false)
    expect(amountComplete({ ...base, currency: 'USD' })).toBe(false)
    expect(amountComplete({ ...base, currency: 'USD', minorUnits: '2' })).toBe(true)
  })
})

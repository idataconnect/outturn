import { describe, expect, it } from 'vitest'

import { covers } from './autoApproval'

const act = { selector: 'act' as const, max_risk: 'low' as const, requires: 'charge', matches: null, skill_name: null }

describe('a policy over a kind of request', () => {
  it('names the skills it reaches, so a currency is not taken for the workspace’s', () => {
    expect(covers(act, ['Bookings'])).toBe('Every “charge” of low risk, in Bookings')
    expect(covers(act)).toBe('Every “charge” of low risk')
  })

})

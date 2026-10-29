import { describe, expect, it } from 'vitest'

import { covers, skillsOf, type BandOperation, type BandsPage } from './autoApproval'

const op = (skill_name: string | null, currency: string, requires = 'charge') =>
  ({ skill_name, currency, requires }) as BandOperation

const page = (...operations: BandOperation[]): BandsPage => ({ operations, currencies: [], unused: [], may_write: true })

const act = { selector: 'act' as const, max_risk: 'low' as const, requires: 'charge', matches: null, skill_name: null }

describe('a policy over a kind of request', () => {
  it('names the skills it reaches, so a currency is not taken for the workspace’s', () => {
    expect(covers(act, ['Bookings'])).toBe('Every “charge” of low risk, in Bookings')
    expect(covers(act)).toBe('Every “charge” of low risk')
  })

  it('finds each skill once, in the currency asked for or any', () => {
    const p = page(op('Bookings', 'GBP'), op('Bookings', 'GBP'), op('Shop', 'USD'), op('Mail', 'USD', 'send'))
    expect(skillsOf(p, 'charge')).toEqual(['Bookings', 'Shop'])
    expect(skillsOf(p, 'charge', 'USD')).toEqual(['Shop'])
    expect(skillsOf(null, 'charge')).toEqual([])
  })
})

import { describe, expect, it } from 'vitest'

import { isClearlyPrivate, webhookUrl } from './webhooks'

describe('isClearlyPrivate', () => {
  it.each([
    'localhost',
    'outturn',
    'api',
    '127.0.0.1',
    '10.0.4.2',
    '172.16.0.1',
    '172.31.255.255',
    '192.168.1.20',
    '169.254.0.1',
    '100.64.0.1',
    '[::1]',
    '[fd12:3456::1]',
    '[fe80::1]',
    'outturn.local',
    'app.localhost',
    'api.internal',
    'outturn.test',
    'nas.home.arpa',
    'Outturn.Local.',
  ])('%s cannot be reached from outside', (host) => {
    expect(isClearlyPrivate(host)).toBe(true)
  })

  it.each([
    'outturn.example.com',
    'hooks.acme.io',
    // Private in practice, but nothing in the name says so, and a false alarm
    // costs more than a missed one.
    'outturn.corp.example.com',
    '8.8.8.8',
    '172.32.0.1',
    '192.169.0.1',
    '[2606:4700::1111]',
  ])('%s is left alone', (host) => {
    expect(isClearlyPrivate(host)).toBe(false)
  })
})

describe('webhookUrl', () => {
  it('puts the endpoint on the origin the browser used', () => {
    expect(webhookUrl('/v1/hooks/abc', 'https://outturn.example.com')).toBe(
      'https://outturn.example.com/v1/hooks/abc',
    )
    expect(webhookUrl('/v1/hooks/abc', 'http://localhost:3000')).toBe(
      'http://localhost:3000/v1/hooks/abc',
    )
  })
})

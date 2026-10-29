/**
 * What a workspace holds against its storage quota. See
 * docs/storage-quota.md.
 */

import { api } from './api'

export type Storage = {
  /** What it holds, in bytes, as last counted. */
  stored_bytes: number
  /** Zero for no limit. */
  quota: number
  /** Whether the holdings have been counted yet; until then no quota applies. */
  counted: boolean
  /** Whether a write would be refused now. */
  full: boolean
}

export const getStorage = () => api<Storage>('/v1/storage')

const UNITS = ['byte', 'kilobyte', 'megabyte', 'gigabyte', 'terabyte', 'petabyte'] as const

/** A size as the reader's locale writes it: `1.5 GB`, `340 kB`, `12 bytes`.
 *  In powers of 1,000, the units the names say. */
export function bytesLabel(bytes: number, locale?: string): string {
  let i = 0
  let n = bytes
  while (n >= 1000 && i < UNITS.length - 1) {
    n /= 1000
    i++
  }
  return new Intl.NumberFormat(locale, {
    style: 'unit',
    unit: UNITS[i],
    unitDisplay: i === 0 ? 'long' : 'short',
    maximumFractionDigits: i === 0 || n >= 100 ? 0 : 1,
  }).format(n)
}

/** The plain words a full workspace is told, wherever a write could start. */
export const FULL =
  'This workspace’s storage is full. Files can still be read and deleted, but not added.'

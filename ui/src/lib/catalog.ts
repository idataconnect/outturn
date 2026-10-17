import { api } from './api'

/** The model catalog this deployment holds, as the API describes it. */
export type HeldCatalog = {
  /** `shipped`, the URL it was fetched from, `upload`, or `file:` and a path. */
  source: string
  fetched_at: string
  providers: number
  models: number
  snapshots: number
  checked_at: string | null
  next_check_at: string | null
  last_error: string | null
  /** Whether this deployment fetches the catalog at all. */
  fetching: boolean
}

export type Checked = { checked: 'kept' | 'unchanged' }

export const heldCatalog = () => api<HeldCatalog>('/v1/platform/catalog')

export const refreshCatalog = () =>
  api<Checked>('/v1/platform/catalog/refresh', { method: 'POST' })

/** A catalog file as chosen: the site's JSON or a reduced one, gzipped or not.
 *  Sent as it is; the API reads all of those. */
export const importCatalog = (file: File) =>
  api<Checked>('/v1/platform/catalog', {
    method: 'POST',
    body: file,
    headers: { 'content-type': 'application/octet-stream' },
  })

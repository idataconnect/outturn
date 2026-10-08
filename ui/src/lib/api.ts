export type Page<T> = { items: T[]; next: string | null }

/**
 * Every row of a paged list, one bounded page at a time.
 *
 * For the lists this app still shows whole -- agents, users, roles and the
 * rest. Taking only the first page cut each of them off at fifty or a hundred
 * with nothing on screen saying there was more. Each request stays bounded;
 * what is unbounded is the list, which is what these pages showed before they
 * were paged. A list that outgrows that wants scrolling or search, not this.
 */
export async function allPages<T>(path: string): Promise<T[]> {
  const items: T[] = []
  let after: string | null = null
  do {
    const joiner = path.includes('?') ? '&' : '?'
    const url: string = after ? `${path}${joiner}after=${encodeURIComponent(after)}` : path
    const page: Page<T> = await api<Page<T>>(url)
    items.push(...page.items)
    after = page.next
  } while (after)
  return items
}

export class ApiError extends Error {
  // Declared and assigned explicitly rather than as a parameter property,
  // which erasableSyntaxOnly disallows.
  readonly status: number

  constructor(status: number, message: string) {
    super(message)
    this.status = status
  }
}

/// The API could not be reached at all — a stopped backend or a dev-server
/// proxy pointing somewhere wrong. Distinct from ApiError so callers never
/// report a connection failure as a rejected credential.
export class NetworkError extends Error {}

/**
 * Calls the API.
 *
 * The session lives in an HttpOnly cookie, so there is no token to attach or
 * store here: `credentials: 'include'` is what carries it, and page JavaScript
 * never sees it.
 *
 * Access tokens are short-lived, so a 401 is usually just an expired one: the
 * refresh cookie is exchanged for a new one and the call retried once. Only if
 * that fails is the session really over, and then `onSessionEnded` hears it.
 */
export async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  return withRefresh(path, () => call<T>(path, init))
}

/** As `api`, for an endpoint that answers with text rather than JSON -- a
 *  skill file, which is prose and stored as it was written. */
export async function apiText(path: string, init: RequestInit = {}): Promise<string> {
  return withRefresh(path, () => call<string>(path, init, 'text'))
}

async function withRefresh<T>(path: string, attempt: () => Promise<T>): Promise<T> {
  try {
    return await attempt()
  } catch (e) {
    if (!isUnauthorized(e) || path === REFRESH_PATH || path === LOGIN_PATH) {
      throw e
    }
    try {
      await refreshSession()
      return await attempt()
    } catch (after) {
      // A refresh refused, or a request still refused with a fresh token: the
      // server has said this session is over, and no retry will change its
      // mind. Said once, here, because otherwise each caller decides alone --
      // and a poller built to wait out a restarting API retries a revoked
      // session every few seconds for as long as the tab is open.
      if (isUnauthorized(after)) endSession()
      throw after
    }
  }
}

const isUnauthorized = (e: unknown) => e instanceof ApiError && e.status === 401

const REFRESH_PATH = '/v1/session/refresh'
// Excluded from refreshing because its 401 is a wrong password, not an expired
// session -- there is no session yet to refresh or to end.
const LOGIN_PATH = '/v1/login'

const sessionEnded = new Set<() => void>()

/** Calls `listener` when the server refuses this session for good. Returns the
 *  unsubscribe. The app's session state is the one listener, and it shows the
 *  sign-in page, which unmounts everything still polling. */
export function onSessionEnded(listener: () => void): () => void {
  sessionEnded.add(listener)
  return () => {
    sessionEnded.delete(listener)
  }
}

function endSession() {
  for (const listener of sessionEnded) listener()
}

/** In-flight refresh, so concurrent 401s trigger only one rotation. */
let refreshInFlight: Promise<void> | null = null

function refreshSession(): Promise<void> {
  // Rotation invalidates the presented token, so parallel refreshes would
  // look like a replay and revoke the whole family.
  refreshInFlight ??= call<unknown>(REFRESH_PATH, { method: 'POST' })
    .then(() => undefined)
    .finally(() => {
      refreshInFlight = null
    })
  return refreshInFlight
}

async function call<T>(
  path: string,
  init: RequestInit = {},
  as: 'json' | 'text' = 'json',
): Promise<T> {
  const headers = new Headers(init.headers)
  if (init.body && !headers.has('content-type')) {
    headers.set('content-type', 'application/json')
  }

  let response: Response
  try {
    response = await fetch(path, {
      ...init,
      headers,
      credentials: 'include',
    })
  } catch (cause) {
    throw new NetworkError(
      'Could not reach the API. Is the backend running, and does the dev-server proxy point at it?',
      { cause },
    )
  }

  // A dev-server proxy that cannot reach its target answers with an HTML
  // error page, which must not be mistaken for an API response.
  if (response.status === 504 || response.headers.get('content-type')?.includes('text/html')) {
    throw new NetworkError(
      'The dev-server proxy could not reach the API. Check VITE_API_PORT matches skaffold.',
    )
  }

  if (!response.ok) {
    const detail = await response.text().catch(() => '')
    throw new ApiError(response.status, detail || response.statusText)
  }

  if (response.status === 204) return undefined as T
  if (as === 'text') return (await response.text()) as T
  return (await response.json()) as T
}

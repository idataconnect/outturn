export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message)
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
 * that fails is the session really over.
 */
export async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  try {
    return await call<T>(path, init)
  } catch (e) {
    if (!(e instanceof ApiError) || e.status !== 401 || path === REFRESH_PATH) {
      throw e
    }
    await refreshSession()
    return await call<T>(path, init)
  }
}

const REFRESH_PATH = '/v1/session/refresh'

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

async function call<T>(path: string, init: RequestInit = {}): Promise<T> {
  const headers = new Headers(init.headers)
  if (init.body && !headers.has('content-type')) {
    headers.set('content-type', 'application/json')
  }

  let response: Response
  try {
    response = await fetch(`/api${path}`, {
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
  return (await response.json()) as T
}

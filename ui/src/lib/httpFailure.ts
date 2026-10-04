/** Words for the statuses a fetch most often fails with. */
const REASONS: Record<number, string> = {
  400: 'Bad Request',
  401: 'Unauthorized',
  403: 'Forbidden',
  404: 'Not Found',
  409: 'Conflict',
  422: 'Unprocessable Content',
  429: 'Too Many Requests',
  500: 'Internal Server Error',
  502: 'Bad Gateway',
  503: 'Service Unavailable',
  504: 'Gateway Timeout',
}

/**
 * Why a fetch that went through still failed: the server answered with an
 * error status. The tool did its job -- the request went out and an answer
 * came back, which is why the model is not told it errored -- but the call did
 * not do what it was for, and a green tick beside a 401 says the opposite.
 */
export function httpFailure(details: unknown): string | undefined {
  if (typeof details !== 'string' || details === '') return undefined
  try {
    const { status } = JSON.parse(details) as { status?: unknown }
    if (typeof status !== 'number' || status < 400) return undefined
    const reason = REASONS[status]
    return `The server answered ${status}${reason ? ` ${reason}` : ''}.`
  } catch {
    return undefined
  }
}

/**
 * How long an agent has left to sleep, as the banner says it: "for 8 more
 * minutes", or "until 14:10" once it is far enough off that a clock time says
 * more than a count.
 */
export function remaining(until: string, now: number): string {
  const at = new Date(until)
  const left = at.getTime() - now
  if (!Number.isFinite(left)) return ''
  if (left <= 0) return '— waking up'
  const seconds = Math.round(left / 1000)
  if (seconds < 60) return `for ${seconds} more second${seconds === 1 ? '' : 's'}`
  const minutes = Math.round(seconds / 60)
  if (minutes < 90) return `for ${minutes} more minute${minutes === 1 ? '' : 's'}`
  const time = at.toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' })
  if (at.toDateString() === new Date(now).toDateString()) return `until ${time}`
  const day = at.toLocaleDateString(undefined, { weekday: 'short', day: 'numeric', month: 'short' })
  return `until ${day}, ${time}`
}

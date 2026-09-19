import type { ToolCallMessagePartComponent } from '@assistant-ui/react'
import ToolCall from './ToolCall'

/**
 * What the clock answered, as a person reads a date.
 *
 * The tool returns `{now, weekday, timezone, abbreviation}` -- four keys that
 * mean one thing, which is what a renderer is for. "Wednesday, 19 September
 * 2026 at 2:14 pm AEST" is the sentence; the JSON is the machinery.
 */
function readableTime(details: unknown): string {
  if (typeof details !== 'string' || details === '') return ''
  try {
    const parsed: unknown = JSON.parse(details)
    if (typeof parsed !== 'object' || parsed === null) return ''
    const { now, weekday, abbreviation } = parsed as Record<string, unknown>
    if (typeof now !== 'string' || now === '') return ''

    // Split off the offset and read the date and time out of the timestamp
    // itself, rather than through `Date`. A parsed date renders in the
    // browser's zone, and this tool exists precisely because that is the wrong
    // zone: the clock belongs to the user the turn is for, who may be nowhere
    // near whoever is reading the transcript. Reformatting it locally would
    // quietly contradict the agent, which is worse than showing the raw
    // string.
    const match = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})/.exec(now)
    if (!match) return now

    const [, year, month, day, hour, minute] = match
    const months = [
      'January',
      'February',
      'March',
      'April',
      'May',
      'June',
      'July',
      'August',
      'September',
      'October',
      'November',
      'December',
    ]
    const monthName = months[Number(month) - 1] ?? month
    const hours = Number(hour)
    const suffix = hours < 12 ? 'am' : 'pm'
    const twelve = hours % 12 === 0 ? 12 : hours % 12

    const date = `${Number(day)} ${monthName} ${year}`
    const time = `${twelve}:${minute} ${suffix}`
    const zone = typeof abbreviation === 'string' && abbreviation !== '' ? ` ${abbreviation}` : ''
    const day_name = typeof weekday === 'string' && weekday !== '' ? `${weekday}, ` : ''

    return `${day_name}${date} at ${time}${zone}`
  } catch {
    // Not JSON: nothing to read out.
    return ''
  }
}

/**
 * The clock, shown as the answer rather than as a call.
 *
 * A reader checking whether the agent knew what day it was wants the date it
 * was given, which is the one thing the verb cannot tell them.
 */
const ToolClock: ToolCallMessagePartComponent = (props) => {
  const when = readableTime(props.args?.details)
  return (
    <ToolCall {...props}>
      {when && (
        <p className="mt-1 text-[11px] text-surface-700 dark:text-surface-300">{when}</p>
      )}
    </ToolCall>
  )
}

export default ToolClock

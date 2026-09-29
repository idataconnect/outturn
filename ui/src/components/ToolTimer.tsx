import type { ToolCallMessagePartComponent } from '@assistant-ui/react'
import ToolCall from './ToolCall'

/** A timer as the platform described it. */
type Timer = { id?: string; when?: string; in?: string; reason?: string }

type Answer = {
  when?: string
  in?: string
  timers?: Timer[]
  other_timers?: Timer[]
  cancelled?: Timer
}

function read(details: unknown): Answer | null {
  if (typeof details !== 'string' || details === '') return null
  try {
    const parsed: unknown = JSON.parse(details)
    return typeof parsed === 'object' && parsed !== null ? (parsed as Answer) : null
  } catch {
    return null
  }
}

/** "Tuesday 29 September at 3:12 pm PDT (in 59 minutes)". */
function moment(t: Timer): string {
  if (!t.when) return ''
  return t.in ? `${t.when} (in ${t.in})` : t.when
}

function TimerList({ timers }: { timers: Timer[] }) {
  return (
    <ul className="mt-0.5 space-y-0.5">
      {timers.map((t, i) => (
        <li key={t.id ?? i}>
          {moment(t)}
          {t.reason && <span className="text-surface-600 dark:text-surface-400"> — {t.reason}</span>}
        </li>
      ))}
    </ul>
  )
}

/**
 * Sleeps and timers, shown as when they fire.
 *
 * The confirmation is the point. A timer set for the wrong hour looks exactly
 * like a right one from its verb, and the only place a person can catch it is
 * here -- so the moment is said in full, day and zone, in the words the
 * platform computed rather than anything the browser recalculates in its own
 * zone. And a timer set while others are pending says so, because setting one
 * never replaces another and the reader should see that there are two.
 */
const ToolTimer: ToolCallMessagePartComponent = (props) => {
  const answer = read(props.args?.details)
  const line = 'mt-1 text-[11px] text-surface-700 dark:text-surface-300'
  if (!answer) return <ToolCall {...props} />

  let body = null
  switch (props.toolName) {
    case 'sleep':
      body = answer.when && <p className={line}>Until {moment(answer)}</p>
      break
    case 'set_timer':
      body = answer.when && (
        <>
          <p className={line}>Fires {moment(answer)}</p>
          {(answer.other_timers?.length ?? 0) > 0 && (
            <div className={line}>
              Also pending:
              <TimerList timers={answer.other_timers ?? []} />
            </div>
          )}
        </>
      )
      break
    case 'list_timers':
      body = answer.timers && (
        <div className={line}>
          {answer.timers.length === 0 ? 'No timers pending' : <TimerList timers={answer.timers} />}
        </div>
      )
      break
    case 'cancel_timer':
      body = answer.cancelled && (
        <p className={line}>
          Cancelled {answer.cancelled.when}
          {answer.cancelled.reason && ` — ${answer.cancelled.reason}`}
        </p>
      )
      break
  }
  return <ToolCall {...props}>{body}</ToolCall>
}

export default ToolTimer

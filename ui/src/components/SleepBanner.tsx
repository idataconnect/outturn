import { useEffect, useState } from 'react'
import { AlarmClock, Moon } from 'lucide-react'

import { wakeSession, type Asleep } from '../lib/chat'
import { remaining } from '../lib/sleep'

/**
 * The agent is asleep: why, until when, and a way to end it now.
 *
 * The reason comes first because it is the agent's own account of what it is
 * waiting for, and it is what a person needs to decide whether waking it early
 * makes sense. Messages sent meanwhile are kept and answered together when it
 * wakes, which the line says so nobody sends the same thing three times.
 */
export default function SleepBanner({
  sessionId,
  asleep,
  onWoken,
}: {
  sessionId: string
  asleep: Asleep
  onWoken: () => void
}) {
  const now = useNow(asleep.until)
  const [waking, setWaking] = useState(false)
  const [error, setError] = useState<string | null>(null)

  async function wake() {
    setWaking(true)
    setError(null)
    try {
      await wakeSession(sessionId)
      onWoken()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Could not wake it')
    } finally {
      setWaking(false)
    }
  }

  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
      <Moon size={14} aria-hidden className="shrink-0 text-brand-600 dark:text-brand-400" />
      <p className="min-w-0 flex-1 text-sm text-surface-800 dark:text-surface-200" role="status">
        <span className="font-medium">Asleep</span> {remaining(asleep.until, now)}
        {asleep.reason && (
          <span className="text-surface-600 dark:text-surface-400"> — {asleep.reason}</span>
        )}
        <span className="block text-xs text-surface-600 dark:text-surface-400">
          Anything you send now is kept, and answered together when it wakes.
        </span>
      </p>
      <button
        type="button"
        onClick={() => void wake()}
        disabled={waking}
        className="inline-flex items-center gap-1 rounded-md border border-surface-300 bg-white px-2.5 py-1 text-xs font-medium text-surface-800 hover:bg-surface-50 disabled:opacity-60 dark:border-surface-600 dark:bg-surface-900 dark:text-surface-200 dark:hover:bg-surface-800"
      >
        <AlarmClock size={12} aria-hidden />
        {waking ? 'Waking…' : 'Wake now'}
      </button>
      {error && (
        <p className="w-full text-xs text-red-600 dark:text-red-400" role="alert">
          {error}
        </p>
      )}
    </div>
  )
}

/**
 * The time, ticking while there is a countdown worth showing.
 *
 * Every second in the last two minutes and every fifteen before that: a
 * countdown of hours changing its last digit each second is motion with no
 * news in it.
 */
function useNow(until: string): number {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    const left = new Date(until).getTime() - now
    if (!Number.isFinite(left) || left <= 0) return
    const id = setTimeout(() => setNow(Date.now()), left < 120_000 ? 1_000 : 15_000)
    return () => clearTimeout(id)
  }, [until, now])
  return now
}

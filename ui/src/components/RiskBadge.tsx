import type { Risk } from '../lib/money'

const tone: Record<Risk, string> = {
  low: 'border-emerald-300 text-emerald-800 dark:border-emerald-800 dark:text-emerald-300',
  medium: 'border-amber-300 text-amber-800 dark:border-amber-800 dark:text-amber-300',
  high: 'border-red-300 text-red-800 dark:border-red-800 dark:text-red-300',
}

/**
 * The risk a request works out to, from its operation and its amount. Written
 * out as a word as well as a color, since a color alone says nothing to
 * somebody who cannot tell the three apart.
 */
export default function RiskBadge({ risk }: { risk: Risk }) {
  return (
    <span
      className={`inline-flex shrink-0 items-center rounded border px-1.5 text-[11px] font-medium leading-4 ${tone[risk]}`}
    >
      {risk === 'low' ? 'Low risk' : risk === 'medium' ? 'Medium risk' : 'High risk'}
    </span>
  )
}

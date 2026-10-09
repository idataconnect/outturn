import { Clock, Webhook } from 'lucide-react'

import type { TriggerKind } from '../lib/chat'

/** The mark for what started something on its own, the same everywhere. */
export default function TriggerIcon({
  kind,
  className = 'w-3.5 h-3.5',
}: {
  kind: TriggerKind
  className?: string
}) {
  const Icon = kind === 'schedule' ? Clock : Webhook
  return <Icon className={className} aria-hidden />
}

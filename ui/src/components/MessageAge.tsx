/**
 * How long ago a message was written, shown under it while the pointer is on it.
 *
 * The figure comes from `useMessageAge`, which derives it from the message id
 * when the pointer arrives and never stores it. This draws it, and holds its
 * space whether or not anything is in it -- a label that took up room only when
 * shown would shove the conversation around as the pointer moved.
 *
 * Fading in is slower than fading out: appearing gently reads as the interface
 * offering something, while lingering on the way out reads as lag.
 */
export default function MessageAge({
  phrase,
  shown,
  align = 'left',
}: {
  phrase: string | null
  shown: boolean
  align?: 'left' | 'right'
}) {
  return (
    <span
      className={`pointer-events-none mt-0.5 block select-none text-[10px] text-surface-400 transition-opacity ease-out dark:text-surface-500 ${
        align === 'right' ? 'text-right' : 'text-left'
      } ${shown ? 'opacity-100 duration-300' : 'opacity-0 duration-150'}`}
      aria-hidden={!shown}
    >
      {/* A non-breaking space keeps the line's height when there is no figure,
          and keeps the last one mounted while it fades out -- a transition
          needs something on screen to act on. */}
      {phrase ?? ' '}
    </span>
  )
}

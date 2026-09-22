import { useEffect, useRef, useState } from 'react'

import { useReducedMotion } from '../lib/useReducedMotion'

/**
 * The mark a reply wears while its turn is still running.
 *
 * Between a tool call being set up and its first result nothing streams, and
 * a reply that has already drawn some text sits there looking finished. The
 * stop button is the only thing that disagrees, and it is at the other end of
 * the page. This says, next to the words themselves, that more is coming.
 *
 * Deliberately not the `Loader` that `StatusLine` spins: that one means
 * "waiting for the model", said under the prompt before a reply exists. A
 * spinner in both places would collapse two different states into one shape.
 * Three dots carrying a travelling swell read as speech continuing rather
 * than as a machine turning, which is the honest description -- the turn is
 * mid-sentence, not stuck. Whichever dot is widest also wears the accent the
 * theme reserves for the mark, so the colour travels with the swell rather
 * than the whole row changing hue at once.
 */
/** How long the dots take to draw out into the line.
 *
 * Slow enough to watch. The first version ran in 420ms with a sharp ease, and
 * most of that was spent already arrived -- what read as a bang rather than a
 * movement. */
const SETTLE_MS = 900

export default function Working({
  done = false,
  leaving = false,
}: {
  done?: boolean
  /** This reply is no longer the newest, so the line is on its way out. It
   *  fades rather than vanishing: a mark that blinked off would draw the eye
   *  to the wrong place just as a new reply starts arriving below it. */
  leaving?: boolean
}) {
  // SMIL is not covered by `prefers-reduced-motion`, so the only way to
  // honour it for the swell is not to draw the animation at all. The colour
  // is CSS on each dot and stops itself -- see index.css.
  const reduced = useReducedMotion()

  // The settle runs once, on the turn ending, and then the mark holds as a
  // line. Kept here rather than driven by the parent because the parent knows
  // only whether a turn is running: a mark that vanished the moment it
  // stopped would have nothing to finish with, which is why this component
  // owns its own ending.
  const [settled, setSettled] = useState(done)
  const wasDone = useRef(done)
  useEffect(() => {
    if (!done || wasDone.current) return
    wasDone.current = true
    if (reduced) {
      // No travel for somebody who asked for none: the line is simply what
      // is there once the turn ends.
      setSettled(true)
      return
    }
    // A little past the animation rather than exactly on it: when `settled`
    // flips, the SMIL elements unmount and the attributes below take over.
    // Landing that on the same frame the animation ends risks reading the
    // frozen value a frame early, which snaps.
    const timer = setTimeout(() => setSettled(true), SETTLE_MS + 60)
    return () => clearTimeout(timer)
  }, [done, reduced])

  const settling = done && !settled && !reduced

  return (
    <span
      className={`mt-1 inline-flex items-center${leaving ? ' working-leaving' : ''}`}
      title={done ? 'Finished' : 'Working'}
      role="status"
      aria-label={done ? 'Finished' : 'Working'}
    >
      <svg width={26} height={10} viewBox="0 0 26 10" aria-hidden focusable="false">
        {/* The turn is over, and the mark says so by becoming a line: each dot
            stretches sideways from where it stands until the three meet. A
            shape rather than a colour, because a colour would have to mean
            something -- green would claim the turn succeeded, which this
            cannot know, and a reply that ended badly would wear it too.

            Held rather than faded. A mark that disappeared would leave a
            finished reply looking like one still being written, which is the
            distinction this draws. */}
        {[3, 13, 23].map((x, i) => (
          // Drawn as a rounded rect rather than a circle so there is one
          // shape throughout: a dot is this at its narrowest, and widening it
          // is the whole animation. Swapping a circle for a rect halfway
          // would be two shapes pretending to be one.
          <rect
            key={x}
            className={
              settling || settled ? 'working-dot working-dot-settling' : 'working-dot'
            }
            // The geometry the settle animates *to* is declared here as CSS
            // custom properties rather than as attributes, because the
            // animation has to interpolate the attributes themselves and a
            // keyframe cannot read a per-dot value any other way.
            style={{
              animationDelay: settling || settled ? '0s' : `${i * 0.4}s`,
              ['--dot-x' as string]: `${x - (reduced ? 2.6 : 2)}`,
              ['--line-x' as string]: `${x - 5}`,
            }}
            x={x - (reduced ? 2.6 : 2)}
            y={5 - (reduced ? 2.6 : 2)}
            width={(reduced ? 2.6 : 2) * 2}
            height={(reduced ? 2.6 : 2) * 2}
            rx={reduced ? 2.6 : 2}
            opacity={reduced ? 0.9 : 0.6}
          >
            {/* One keyframe set, three dots, staggered by delay: the swell
                passes along the row rather than all three breathing together,
                which is what makes it read as travelling. */}
            {!reduced && !settling && !settled && (
              <>
                <animate
                  attributeName="width"
                  values="4;6.8;4"
                  dur="1.2s"
                  begin={`${i * 0.4}s`}
                  repeatCount="indefinite"
                  calcMode="spline"
                  keySplines="0.4 0 0.6 1;0.4 0 0.6 1"
                  keyTimes="0;0.5;1"
                />
                <animate
                  attributeName="height"
                  values="4;6.8;4"
                  dur="1.2s"
                  begin={`${i * 0.4}s`}
                  repeatCount="indefinite"
                  calcMode="spline"
                  keySplines="0.4 0 0.6 1;0.4 0 0.6 1"
                  keyTimes="0;0.5;1"
                />
                {/* x and y follow so the dot swells about its centre rather
                    than growing down and to the right. */}
                <animate
                  attributeName="x"
                  values={`${x - 2};${x - 3.4};${x - 2}`}
                  dur="1.2s"
                  begin={`${i * 0.4}s`}
                  repeatCount="indefinite"
                  calcMode="spline"
                  keySplines="0.4 0 0.6 1;0.4 0 0.6 1"
                  keyTimes="0;0.5;1"
                />
                <animate
                  attributeName="y"
                  values="3;1.6;3"
                  dur="1.2s"
                  begin={`${i * 0.4}s`}
                  repeatCount="indefinite"
                  calcMode="spline"
                  keySplines="0.4 0 0.6 1;0.4 0 0.6 1"
                  keyTimes="0;0.5;1"
                />
                <animate
                  attributeName="rx"
                  values="2;3.4;2"
                  dur="1.2s"
                  begin={`${i * 0.4}s`}
                  repeatCount="indefinite"
                  calcMode="spline"
                  keySplines="0.4 0 0.6 1;0.4 0 0.6 1"
                  keyTimes="0;0.5;1"
                />
                <animate
                  attributeName="opacity"
                  values="0.6;1;0.6"
                  dur="1.2s"
                  begin={`${i * 0.4}s`}
                  repeatCount="indefinite"
                  calcMode="spline"
                  keySplines="0.4 0 0.6 1;0.4 0 0.6 1"
                  keyTimes="0;0.5;1"
                />
              </>
            )}
          </rect>
        ))}
      </svg>
    </span>
  )
}

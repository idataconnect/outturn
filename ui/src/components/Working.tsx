import { useEffect, useRef, useState } from 'react'

import { useReducedMotion } from '../lib/useReducedMotion'

/**
 * The mark a turn wears, from the moment it is asked for until it is done.
 *
 * One shape for the whole life of a turn, changing character rather than
 * being swapped for a different widget at each step. The row of three says
 * where the turn is by *how* it moves:
 *
 *   held     the three breathe together, slowly, dim -- nothing is happening
 *            to it yet, it is in a queue or waiting on a model that has not
 *            started
 *   running  the swell travels along the row -- words are arriving, and the
 *            movement goes somewhere the way the reply does
 *   done     the three draw out into a line and hold
 *
 * Breathing in unison and a travelling swell are the distinction the whole
 * thing rests on: one says "held", the other says "advancing". A spinner
 * cannot make that distinction -- it turns at the same rate whether anything
 * is happening or not, which is why the one that used to sit under the
 * prompt is gone and this covers both ends.
 *
 * Whichever dot is widest wears the accent the theme reserves for the mark,
 * so in the running phase the colour travels with the swell rather than the
 * whole row changing hue at once. In the held phase there is no travel, so
 * all three share the colour: the row pulses as one object.
 */

/** How long the dots take to draw out into the line.
 *
 * Slow enough to watch. The first version ran in 420ms with a sharp ease, and
 * most of that was spent already arrived -- what read as a bang rather than a
 * movement. */
const SETTLE_MS = 900

/**
 * Where a turn is. Deliberately fewer names than `MessageStatus` has: the
 * mark draws movement, and `queued`, `steering` and `waiting` are all the
 * same movement because they are all the same news to a reader -- something
 * has your message and nothing has come back. What separates them belongs in
 * the tooltip, not in the animation.
 */
export type WorkingPhase = 'held' | 'running' | 'done'

export default function Working({
  phase = 'running',
  label,
}: {
  phase?: WorkingPhase
  /** What to call this state for a screen reader and on hover. The phase
   *  says how the mark moves; this says what it means, which is the part
   *  that differs between a queued message and one whose model is slow. */
  label?: string
}) {
  // SMIL is not covered by `prefers-reduced-motion`, so the only way to
  // honour it for the swell is not to draw the animation at all. The colour
  // is CSS on each dot and stops itself -- see index.css.
  const reduced = useReducedMotion()

  const done = phase === 'done'

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
  // The travelling swell, which belongs to `running` alone. A held turn gets
  // the CSS pulse instead: same three dots, no stagger, so the row breathes
  // as one object rather than passing a wave along itself.
  const travelling = phase === 'running' && !reduced && !settling && !settled
  const held = phase === 'held' && !reduced

  const meaning = label ?? (done ? 'Finished' : phase === 'held' ? 'Waiting' : 'Working')

  return (
    <span
      className="mt-1 inline-flex items-center"
      title={meaning}
      role="status"
      aria-label={meaning}
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
              settling || settled
                ? 'working-dot working-dot-settling'
                : held
                  ? 'working-dot working-dot-held'
                  : 'working-dot'
            }
            // The geometry the settle animates *to* is declared here as CSS
            // custom properties rather than as attributes, because the
            // animation has to interpolate the attributes themselves and a
            // keyframe cannot read a per-dot value any other way.
            style={{
              // No stagger while held: the row breathes together, which is
              // what makes it read as one object waiting rather than as
              // something moving along. The settle has none either, for the
              // same reason it has to start from where the dots already are.
              animationDelay: settling || settled || held ? '0s' : `${i * 0.4}s`,
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
            {travelling && (
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

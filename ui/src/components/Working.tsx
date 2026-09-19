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
 * mid-sentence, not stuck.
 */
export default function Working() {
  // SMIL is not covered by `prefers-reduced-motion`, so the only way to
  // honour it for the swell is not to draw the animation at all. The colour
  // underneath is CSS and stops itself -- see index.css.
  const reduced = useReducedMotion()

  return (
    <span
      // The colour is a class rather than a Tailwind text-* utility: it
      // travels between the two ends of the logo's gradient, and the dots
      // below are filled with `currentColor` so they follow it. Light and
      // dark pick different stops -- see index.css.
      className="working-hue mt-1 inline-flex items-center"
      title="Working"
      role="status"
      aria-label="Working"
    >
      <svg width={26} height={10} viewBox="0 0 26 10" aria-hidden focusable="false">
        {/* One keyframe set, three dots, staggered by delay: the swell passes
            along the row rather than all three breathing together, which is
            what makes it read as travelling. Written as SMIL rather than CSS
            because it rides with the element -- no keyframes to declare in a
            stylesheet that has none of its own, and nothing left behind if
            this component goes away. */}
        {[3, 13, 23].map((x, i) => (
          // Held at the swell's midpoint when movement is unwanted: three
          // steady dots in the reply's colour still say something is coming,
          // which is the whole job. Drawn at full strength rather than the
          // resting 0.35, since nothing is going to brighten them.
          <circle
            key={x}
            cx={x}
            cy={5}
            r={reduced ? 2.6 : 2}
            fill="currentColor"
            opacity={reduced ? 0.8 : 0.35}
          >
            {/* `begin` staggers by a third of the cycle each, so the swell
                arrives at each dot in turn and the row never goes fully dark. */}
            {!reduced && (
              <>
                <animate
                  attributeName="r"
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
                  values="0.35;1;0.35"
                  dur="1.2s"
                  begin={`${i * 0.4}s`}
                  repeatCount="indefinite"
                  calcMode="spline"
                  keySplines="0.4 0 0.6 1;0.4 0 0.6 1"
                  keyTimes="0;0.5;1"
                />
              </>
            )}
          </circle>
        ))}
      </svg>
    </span>
  )
}

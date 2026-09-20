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
export default function Working() {
  // SMIL is not covered by `prefers-reduced-motion`, so the only way to
  // honour it for the swell is not to draw the animation at all. The colour
  // is CSS on each dot and stops itself -- see index.css.
  const reduced = useReducedMotion()

  return (
    <span
      className="mt-1 inline-flex items-center"
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
          //
          // `fill` is left to the stylesheet rather than set here: the colour
          // is a theme token, and a value resolved in the component would
          // stop following a theme that replaced it.
          <circle
            key={x}
            className="working-dot"
            // The same stagger the swell uses, so the dot wearing the accent
            // is the dot that is widest rather than one trailing behind it.
            style={{ animationDelay: `${i * 0.4}s` }}
            cx={x}
            cy={5}
            r={reduced ? 2.6 : 2}
            opacity={reduced ? 0.9 : 0.6}
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
                {/* A shallower fade than the colour version needed. There,
                    opacity was the only thing saying which dot was active;
                    here the accent says it, and dropping the resting two to
                    0.35 only leaves their teal looking muddy. */}
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
          </circle>
        ))}
      </svg>
    </span>
  )
}

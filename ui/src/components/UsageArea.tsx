import { useId, useRef, useState } from 'react'

import {
  CACHE_READ,
  KINDS,
  compact,
  dayLabel,
  dayLabelLong,
  exact,
  niceMax,
  seriesColor,
  type Bucket,
} from '../lib/viz'
import { useDarkMode } from '../lib/useDarkMode'

export type { Bucket }

const VIEW_W = 720
const VIEW_H = 220
const PAD = { top: 12, right: 12, bottom: 24, left: 48 }

/**
 * Tokens per day, stacked by kind.
 *
 * Drawn as SVG by hand rather than with a charting library: the app has no
 * chart dependency, and the one thing a library would buy here -- the hover
 * layer -- is a crosshair over a day index, which is the easy part.
 *
 * The stack is a part-to-whole over time, so the bands carry categorical
 * colour and the legend is always present; the tooltip carries the figures, so
 * nothing is encoded by colour alone.
 */
export default function UsageArea({ buckets }: { buckets: Bucket[] }) {
  const dark = useDarkMode()
  const clip = useId()
  const svg = useRef<SVGSVGElement>(null)
  const [hover, setHover] = useState<number | null>(null)

  // Which kinds this window actually contains. A band that is zero everywhere
  // is not drawn and not given a legend entry: an empty band still takes a
  // colour slot, and a legend that lists "Cache write" for a provider that has
  // never reported one teaches the reader something untrue about their bill.
  const present = KINDS.filter((k) => buckets.some((b) => b[k.key] > 0))
  const kinds = present.length > 0 ? present : [KINDS[0]]

  const totals = buckets.map((b) => kinds.reduce((sum, k) => sum + b[k.key], 0))
  const max = niceMax(Math.max(...totals, 1))

  const plotW = VIEW_W - PAD.left - PAD.right
  const plotH = VIEW_H - PAD.top - PAD.bottom
  // A single bucket has no width to interpolate across, so it sits in the
  // middle rather than collapsing onto the left edge.
  const x = (i: number) =>
    buckets.length === 1 ? PAD.left + plotW / 2 : PAD.left + (i / (buckets.length - 1)) * plotW
  const y = (v: number) => PAD.top + plotH - (v / max) * plotH

  // Cumulative tops, band by band, so each band is drawn against the one below.
  const tops: number[][] = []
  let running = buckets.map(() => 0)
  for (const kind of kinds) {
    running = running.map((sum, i) => sum + buckets[i][kind.key])
    tops.push([...running])
  }

  // Cache reads against their own maximum, drawn as a line in the muted ink
  // rather than a sixth series colour: it is context for the stack, not
  // another member of it, and a hue would say they were comparable.
  const cacheReads = buckets.map((b) => b[CACHE_READ.key])
  const cacheMax = niceMax(Math.max(...cacheReads, 1))
  const hasCache = cacheReads.some((v) => v > 0)
  // Confined to the upper half of the plot so it never tangles with the stack
  // below it. Its own scale is stated in its own label, so nothing here
  // pretends the two share an axis.
  const cacheY = (v: number) => PAD.top + (1 - v / cacheMax) * (plotH * 0.45)

  const ticks = [0, max / 2, max]
  const axis = dark ? '#3a3a35' : '#e6e6e2'
  const ink = dark ? '#a3a39a' : '#6b6b63'
  // The 2px ring that keeps a marker legible where it crosses a band.
  const surface = dark ? '#1a1a17' : '#ffffff'

  // A window with nothing but cache reads is not an empty window.
  const empty = totals.every((t) => t === 0) && !cacheReads.some((v) => v > 0)

  function onMove(event: React.PointerEvent<SVGSVGElement>) {
    const box = svg.current?.getBoundingClientRect()
    if (!box || buckets.length === 0) return
    // The SVG scales to its container, so a client x has to come back through
    // the viewBox before it means anything in plot coordinates.
    const local = ((event.clientX - box.left) / box.width) * VIEW_W
    const ratio = (local - PAD.left) / plotW
    const index = Math.round(ratio * (buckets.length - 1))
    setHover(Math.min(buckets.length - 1, Math.max(0, index)))
  }

  const active = hover === null ? null : buckets[hover]

  return (
    <div className="relative">
      <svg
        ref={svg}
        viewBox={`0 0 ${VIEW_W} ${VIEW_H}`}
        className="w-full h-auto touch-none"
        role="img"
        aria-label={`Tokens per day, stacked by kind, over ${buckets.length} days. The table below carries the same figures.`}
        onPointerMove={onMove}
        onPointerLeave={() => setHover(null)}
      >
        <defs>
          <clipPath id={clip}>
            <rect x={PAD.left} y={PAD.top} width={plotW} height={plotH} />
          </clipPath>
        </defs>

        {/* Gridlines: hairline, solid, one step off the surface. They sit
            under the data and are never the loudest thing on the panel. */}
        {ticks.map((t) => (
          <g key={t}>
            <line x1={PAD.left} x2={VIEW_W - PAD.right} y1={y(t)} y2={y(t)} stroke={axis} strokeWidth={1} />
            <text
              x={PAD.left - 8}
              y={y(t) + 4}
              textAnchor="end"
              fontSize={11}
              fill={ink}
              style={{ fontVariantNumeric: 'tabular-nums' }}
            >
              {compact(Math.round(t))}
            </text>
          </g>
        ))}

        {!empty && (
          <g clipPath={`url(#${clip})`}>
            {kinds.map((kind, band) => {
              const upper = tops[band]
              const lower = band === 0 ? buckets.map(() => 0) : tops[band - 1]
              const forward = upper.map((v, i) => `${x(i)},${y(v)}`).join(' L ')
              const back = [...lower].reverse().map((v, i) => {
                const idx = lower.length - 1 - i
                return `${x(idx)},${y(v)}`
              })
              const color = seriesColor(band, dark)
              return (
                <g key={kind.key}>
                  {/* The band as a wash, with its own top drawn as a 2px line
                      -- the line is what separates it from the band above,
                      rather than a stroke drawn around the whole shape. */}
                  <path d={`M ${forward} L ${back.join(' L ')} Z`} fill={color} fillOpacity={0.18} />
                  <path
                    d={`M ${upper.map((v, i) => `${x(i)},${y(v)}`).join(' L ')}`}
                    fill="none"
                    stroke={color}
                    strokeWidth={2}
                    strokeLinejoin="round"
                    strokeLinecap="round"
                  />
                </g>
              )
            })}
          </g>
        )}

        {/* Cache reads: dashed, muted, and labelled with their own maximum.
            Dashed because it is the one mark on this panel that does not share
            the axis beside it, and a reader must be able to see that at a
            glance rather than discover it in the legend. */}
        {hasCache && !empty && (
          <g clipPath={`url(#${clip})`}>
            <path
              d={`M ${cacheReads.map((v, i) => `${x(i)},${cacheY(v)}`).join(' L ')}`}
              fill="none"
              stroke={ink}
              strokeWidth={2}
              strokeDasharray="4 3"
              strokeLinejoin="round"
              strokeLinecap="round"
            />
          </g>
        )}

        {/* The crosshair, and a marker per band at the hovered day. */}
        {active && !empty && (
          <g>
            <line
              x1={x(hover!)}
              x2={x(hover!)}
              y1={PAD.top}
              y2={PAD.top + plotH}
              stroke={ink}
              strokeWidth={1}
            />
            {kinds.map((kind, band) => (
              <circle
                key={kind.key}
                cx={x(hover!)}
                cy={y(tops[band][hover!])}
                r={4}
                fill={seriesColor(band, dark)}
                stroke={surface}
                strokeWidth={2}
              />
            ))}
            {hasCache && (
              <circle
                cx={x(hover!)}
                cy={cacheY(cacheReads[hover!])}
                r={4}
                fill={ink}
                stroke={surface}
                strokeWidth={2}
              />
            )}
          </g>
        )}

        {/* Only the ends of the axis are labelled: a tick under every day is
            unreadable at a month's width, and the tooltip names the day the
            reader is actually pointing at. */}
        {buckets.length > 0 && (
          <>
            <text x={PAD.left} y={VIEW_H - 6} fontSize={11} fill={ink}>
              {dayLabel(buckets[0].at)}
            </text>
            <text x={VIEW_W - PAD.right} y={VIEW_H - 6} fontSize={11} fill={ink} textAnchor="end">
              {dayLabel(buckets[buckets.length - 1].at)}
            </text>
          </>
        )}

        {empty && (
          <text
            x={PAD.left + plotW / 2}
            y={PAD.top + plotH / 2}
            textAnchor="middle"
            fontSize={12}
            fill={ink}
          >
            No model calls in this window.
          </text>
        )}
      </svg>

      {/* The legend is always present for two or more bands, so identity never
          rests on colour alone. */}
      {(kinds.length > 1 || hasCache) && (
        <ul className="mt-2 flex flex-wrap gap-x-4 gap-y-1">
          {kinds.map((kind, band) => (
            <li
              key={kind.key}
              className="flex items-center gap-1.5 text-xs text-surface-600 dark:text-surface-400"
            >
              <span
                aria-hidden
                className="inline-block w-2.5 h-2.5 rounded-sm"
                style={{ background: seriesColor(band, dark) }}
              />
              {kind.label}
            </li>
          ))}
          {hasCache && (
            // The separate scale is said here, in words, rather than left for
            // the reader to infer from a line that does not match the axis.
            <li className="flex items-center gap-1.5 text-xs text-surface-600 dark:text-surface-400">
              <svg width="14" height="10" aria-hidden className="shrink-0">
                <line
                  x1="0"
                  y1="5"
                  x2="14"
                  y2="5"
                  stroke={ink}
                  strokeWidth={2}
                  strokeDasharray="4 3"
                />
              </svg>
              Cache read — own scale, to {compact(cacheMax)}
            </li>
          )}
        </ul>
      )}

      {/* Positioned over the plot rather than following the pointer: a tooltip
          that chases the cursor across a month of data is harder to read than
          one that stays where the reader's eye already is. */}
      {active && !empty && (
        <div className="pointer-events-none absolute top-0 right-0 rounded-md border border-surface-200 dark:border-surface-700 bg-white/95 dark:bg-surface-800/95 px-3 py-2 shadow-sm">
          <p className="text-xs font-medium text-surface-900 dark:text-surface-100">
            {dayLabelLong(active.at)}
          </p>
          <p className="text-xs text-surface-600 dark:text-surface-400">
            {exact(active.calls)} {active.calls === 1 ? 'call' : 'calls'}
          </p>
          <ul className="mt-1 space-y-0.5">
            {kinds.map((kind, band) => (
              <li key={kind.key} className="flex items-center gap-2 text-xs">
                <span
                  aria-hidden
                  className="inline-block w-2 h-2 rounded-sm shrink-0"
                  style={{ background: seriesColor(band, dark) }}
                />
                <span className="text-surface-600 dark:text-surface-400">{kind.label}</span>
                <span
                  className="ml-auto text-surface-900 dark:text-surface-100"
                  style={{ fontVariantNumeric: 'tabular-nums' }}
                >
                  {exact(active[kind.key])}
                </span>
              </li>
            ))}
            {hasCache && (
              <li className="flex items-center gap-2 text-xs">
                <svg width="8" height="8" aria-hidden className="shrink-0">
                  <line x1="0" y1="4" x2="8" y2="4" stroke={ink} strokeWidth={2} />
                </svg>
                <span className="text-surface-600 dark:text-surface-400">
                  {CACHE_READ.label}
                </span>
                <span
                  className="ml-auto text-surface-900 dark:text-surface-100"
                  style={{ fontVariantNumeric: 'tabular-nums' }}
                >
                  {exact(active[CACHE_READ.key])}
                </span>
              </li>
            )}
          </ul>
        </div>
      )}
    </div>
  )
}

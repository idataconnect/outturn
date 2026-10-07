import { useId, useRef, useState } from 'react'

import {
  CACHE_READ,
  KINDS,
  cacheHitRate,
  compact,
  dayLabel,
  dayLabelLong,
  exact,
  niceMax,
  seriesColor,
  share,
  type Bucket,
} from '../lib/viz'
import { useDarkMode } from '../lib/useDarkMode'

export type { Bucket }

const VIEW_W = 720
const PAD = { top: 12, right: 12, bottom: 24, left: 48 }
const PLOT_H = 184
// The cache strip beneath the stack: short, because a rate between 0 and 1
// needs less height than a count does to show its movement.
const STRIP_TOP = 6
const STRIP_PLOT_H = 48

/**
 * Tokens per day, stacked by kind, with the share of input served from cache
 * in a strip beneath.
 *
 * Drawn as SVG by hand rather than with a charting library: the app has no
 * chart dependency, and the one thing a library would buy here -- the hover
 * layer -- is a crosshair over a day index, which is the easy part.
 *
 * The stack is a part-to-whole over time, so the bands carry categorical
 * color and the legend is always present; the tooltip carries the figures, so
 * nothing is encoded by color alone. The strip is one series with its own
 * axis and its own title, so it needs neither a hue nor a legend entry.
 */
export default function UsageArea({ buckets }: { buckets: Bucket[] }) {
  const dark = useDarkMode()
  const clip = useId()
  const stripClip = useId()
  const svg = useRef<SVGSVGElement>(null)
  const [hover, setHover] = useState<number | null>(null)

  // Which kinds this window actually contains. A band that is zero everywhere
  // is not drawn and not given a legend entry: an empty band still takes a
  // color slot, and a legend that lists "Cache write" for a provider that has
  // never reported one teaches the reader something untrue about their bill.
  const present = KINDS.filter((k) => buckets.some((b) => b[k.key] > 0))
  const kinds = present.length > 0 ? present : [KINDS[0]]

  const totals = buckets.map((b) => kinds.reduce((sum, k) => sum + b[k.key], 0))
  const max = niceMax(Math.max(...totals, 1))

  // Cache reads, as the share of each day's input they served. See [`KINDS`]
  // for why they sit beneath the stack rather than in it. The strip is left
  // out, like an empty band, for a window whose provider never reported one.
  const hasCache = buckets.some((b) => b[CACHE_READ.key] > 0)
  const rates = buckets.map(cacheHitRate)

  // With the strip present the day labels move under it, so the two panels
  // read as one figure over one x-axis rather than two charts that happen to
  // be stacked.
  const plotW = VIEW_W - PAD.left - PAD.right
  const viewH = PAD.top + PLOT_H + (hasCache ? 8 : PAD.bottom)
  const stripViewH = STRIP_TOP + STRIP_PLOT_H + PAD.bottom

  // A single bucket has no width to interpolate across, so it sits in the
  // middle rather than collapsing onto the left edge.
  const x = (i: number) =>
    buckets.length === 1 ? PAD.left + plotW / 2 : PAD.left + (i / (buckets.length - 1)) * plotW
  const y = (v: number) => PAD.top + PLOT_H - (v / max) * PLOT_H
  const stripY = (rate: number) => STRIP_TOP + (1 - rate) * STRIP_PLOT_H

  // Cumulative tops, band by band, so each band is drawn against the one below.
  const tops: number[][] = []
  let running = buckets.map(() => 0)
  for (const kind of kinds) {
    running = running.map((sum, i) => sum + buckets[i][kind.key])
    tops.push([...running])
  }

  // The rate line, broken wherever a day had no input rather than drawn
  // through it: see `cacheHitRate`. A run of one day has no line to draw, so
  // it is marked with a dot instead of vanishing.
  const runs: number[][] = []
  rates.forEach((rate, i) => {
    if (rate === null) return
    const last = runs.at(-1)
    if (last && last.at(-1) === i - 1) last.push(i)
    else runs.push([i])
  })

  const ticks = [0, max / 2, max]
  const axis = dark ? '#3a3a35' : '#e6e6e2'
  const ink = dark ? '#a3a39a' : '#6b6b63'
  // The 2px ring that keeps a marker legible where it crosses a band.
  const surface = dark ? '#1a1a17' : '#ffffff'

  // A window with nothing but cache reads is not an empty window.
  const empty = totals.every((t) => t === 0) && !hasCache

  // One hover for both panels. They share a width and a viewBox width, so the
  // stack's box maps a pointer over either of them to the same day.
  function onMove(event: React.PointerEvent<HTMLDivElement>) {
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
  const activeRate = hover === null ? null : rates[hover]

  // Only the ends of the axis are labeled: a tick under every day is
  // unreadable at a month's width, and the tooltip names the day the reader
  // is actually pointing at.
  function dayLabels(bottom: number) {
    if (buckets.length === 0) return null
    return (
      <>
        <text x={PAD.left} y={bottom - 6} fontSize={11} fill={ink}>
          {dayLabel(buckets[0].at)}
        </text>
        <text x={VIEW_W - PAD.right} y={bottom - 6} fontSize={11} fill={ink} textAnchor="end">
          {dayLabel(buckets[buckets.length - 1].at)}
        </text>
      </>
    )
  }

  return (
    <div className="relative touch-none" onPointerMove={onMove} onPointerLeave={() => setHover(null)}>
      <svg
        ref={svg}
        viewBox={`0 0 ${VIEW_W} ${viewH}`}
        className="w-full h-auto"
        role="img"
        aria-label={`Tokens per day, stacked by kind, over ${buckets.length} days. The table below carries the same figures.`}
      >
        <defs>
          <clipPath id={clip}>
            <rect x={PAD.left} y={PAD.top} width={plotW} height={PLOT_H} />
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

        {/* The crosshair, and a marker per band at the hovered day. */}
        {active && !empty && (
          <g>
            <line
              x1={x(hover!)}
              x2={x(hover!)}
              y1={PAD.top}
              y2={PAD.top + PLOT_H}
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
          </g>
        )}

        {!hasCache && dayLabels(viewH)}

        {empty && (
          <text
            x={PAD.left + plotW / 2}
            y={PAD.top + PLOT_H / 2}
            textAnchor="middle"
            fontSize={12}
            fill={ink}
          >
            No model calls in this window.
          </text>
        )}
      </svg>

      {/* The legend is always present for two or more bands, so identity never
          rests on color alone. It names the stack only: the strip below is
          titled where it is drawn. */}
      {kinds.length > 1 && (
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
        </ul>
      )}

      {hasCache && (
        <div className="mt-4">
          <p className="text-xs font-medium text-surface-700 dark:text-surface-300">
            Input served from cache
          </p>
          <svg
            viewBox={`0 0 ${VIEW_W} ${stripViewH}`}
            className="mt-1 w-full h-auto"
            role="img"
            aria-label={`Share of each day's input read from cache, over ${buckets.length} days. The table below carries the cache reads it is computed from.`}
          >
            <defs>
              {/* Widened by the stroke so a line at 0% or 100% is not shaved
                  to half its width by the edge it runs along. */}
              <clipPath id={stripClip}>
                <rect x={PAD.left - 4} y={STRIP_TOP - 4} width={plotW + 8} height={STRIP_PLOT_H + 8} />
              </clipPath>
            </defs>

            {[0, 1].map((t) => (
              <g key={t}>
                <line
                  x1={PAD.left}
                  x2={VIEW_W - PAD.right}
                  y1={stripY(t)}
                  y2={stripY(t)}
                  stroke={axis}
                  strokeWidth={1}
                />
                <text
                  x={PAD.left - 8}
                  y={stripY(t) + 4}
                  textAnchor="end"
                  fontSize={11}
                  fill={ink}
                  style={{ fontVariantNumeric: 'tabular-nums' }}
                >
                  {t * 100}%
                </text>
              </g>
            ))}

            <g clipPath={`url(#${stripClip})`}>
              {runs.map((run) =>
                run.length === 1 ? (
                  <circle key={run[0]} cx={x(run[0])} cy={stripY(rates[run[0]]!)} r={3} fill={ink} />
                ) : (
                  <path
                    key={run[0]}
                    d={`M ${run.map((i) => `${x(i)},${stripY(rates[i]!)}`).join(' L ')}`}
                    fill="none"
                    stroke={ink}
                    strokeWidth={2}
                    strokeLinejoin="round"
                    strokeLinecap="round"
                  />
                ),
              )}
            </g>

            {active && (
              <g>
                <line
                  x1={x(hover!)}
                  x2={x(hover!)}
                  y1={STRIP_TOP}
                  y2={STRIP_TOP + STRIP_PLOT_H}
                  stroke={ink}
                  strokeWidth={1}
                />
                {activeRate !== null && (
                  <circle
                    cx={x(hover!)}
                    cy={stripY(activeRate)}
                    r={4}
                    fill={ink}
                    stroke={surface}
                    strokeWidth={2}
                  />
                )}
              </g>
            )}

            {dayLabels(stripViewH)}
          </svg>
        </div>
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
                  className="ml-auto pl-3 text-surface-900 dark:text-surface-100"
                  style={{ fontVariantNumeric: 'tabular-nums' }}
                >
                  {exact(active[CACHE_READ.key])}
                  {activeRate !== null && (
                    <span className="text-surface-500 dark:text-surface-400">
                      {' '}
                      ·{' '}
                      {share(
                        active.cache_read_tokens,
                        active.prompt_tokens + active.cache_read_tokens + active.cache_write_tokens,
                      )}{' '}
                      of input
                    </span>
                  )}
                </span>
              </li>
            )}
          </ul>
        </div>
      )}
    </div>
  )
}

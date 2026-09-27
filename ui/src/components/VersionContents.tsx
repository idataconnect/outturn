import { useMemo, useState } from 'react'
import { ChevronDown, ChevronRight, FileText, GitCompare, Globe } from 'lucide-react'

import { collapse, hasChanges, lineDiff, MAX_LINES, type Change } from '../lib/lineDiff'

/**
 * What a version of a skill actually says, under the row that names it.
 *
 * The history listed ordinals, notes and dates, and a Restore button beside them.
 * Everything a person needs to answer "is this the one I want?" -- the prose the
 * agent was given, the hosts it named, the files it carried -- was on the wire and
 * shown nowhere, so Restore was a button somebody pressed to find out.
 *
 * Two readings, because the question has two shapes. *What did this say* wants the
 * body whole. *What did this change* wants a diff, and against the live version
 * rather than against the one before it: somebody deciding whether to restore is
 * asking what would be different afterwards, and the version before this one is
 * not what they would be leaving.
 */
export default function VersionContents({
  version,
  live,
  isLive,
}: {
  version: { body: string; hosts: string[]; files?: { path: string; bytes: number }[] }
  /** The live version's body, for the diff. Absent when this *is* the live one. */
  live?: string
  isLive: boolean
}) {
  const [shown, setShown] = useState<'closed' | 'body' | 'diff'>('closed')

  // Computed only when a diff is on screen. A page listing twenty versions must
  // not diff twenty bodies to render the list.
  const sections = useMemo(() => {
    if (shown !== 'diff' || live === undefined) return null
    const changes = lineDiff(live, version.body)
    if (changes === null) return 'too-long' as const
    return hasChanges(changes) ? collapse(changes) : 'identical' as const
  }, [shown, live, version.body])

  return (
    <div className="mt-2">
      <div className="flex flex-wrap items-center gap-2">
        <Toggle
          active={shown === 'body'}
          onClick={() => setShown(shown === 'body' ? 'closed' : 'body')}
          icon={FileText}
          label={shown === 'body' ? 'Hide what it said' : 'What it said'}
        />
        {/* No diff against itself: the live version is the thing being compared
            to, and an empty diff of it against itself explains nothing. */}
        {!isLive && live !== undefined && (
          <Toggle
            active={shown === 'diff'}
            onClick={() => setShown(shown === 'diff' ? 'closed' : 'diff')}
            icon={GitCompare}
            label={shown === 'diff' ? 'Hide the difference' : 'Compare with live'}
          />
        )}
      </div>

      {shown === 'body' && (
        <div className="mt-3 space-y-3">
          <Pre>{version.body}</Pre>
          <Carried hosts={version.hosts} files={version.files} />
        </div>
      )}

      {shown === 'diff' && (
        <div className="mt-3 space-y-3">
          {sections === 'identical' && (
            <p className="text-xs text-surface-600 dark:text-surface-400">
              {/* Worth saying rather than showing an empty panel: two versions
                  can differ in their note, hosts or files and say the same words. */}
              The words are the same as the live version. Anything that differs is in
              its hosts, its files or its note.
            </p>
          )}
          {sections === 'too-long' && (
            <p className="text-xs text-surface-600 dark:text-surface-400">
              Too long to compare here — over {MAX_LINES.toLocaleString()} lines. Read what
              it said instead.
            </p>
          )}
          {Array.isArray(sections) && <Diff sections={sections} />}
          <Carried hosts={version.hosts} files={version.files} />
        </div>
      )}
    </div>
  )
}

function Toggle({
  active,
  onClick,
  icon: Icon,
  label,
}: {
  active: boolean
  onClick: () => void
  icon: typeof FileText
  label: string
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-expanded={active}
      className={`inline-flex items-center gap-1.5 rounded-md border px-2 py-1 text-xs font-medium transition-colors ${
        active
          ? 'border-brand-300 bg-brand-50 text-brand-900 dark:border-brand-800 dark:bg-brand-950 dark:text-brand-200'
          : 'border-surface-300 text-surface-700 hover:bg-surface-50 dark:border-surface-700 dark:text-surface-300 dark:hover:bg-surface-800'
      }`}
    >
      {active ? <ChevronDown size={12} aria-hidden /> : <ChevronRight size={12} aria-hidden />}
      <Icon size={12} aria-hidden />
      {label}
    </button>
  )
}

/**
 * Prose as it was written.
 *
 * Not rendered as Markdown. What the agent is given is the source, so the source
 * is what a person comparing versions has to see: rendered, a changed heading
 * level or a broken list looks the same as what it replaced.
 */
function Pre({ children }: { children: string }) {
  return (
    <pre className="max-h-96 overflow-auto rounded-md border border-surface-200 bg-surface-50 p-3 text-xs leading-relaxed whitespace-pre-wrap break-words text-surface-800 dark:border-surface-800 dark:bg-surface-950 dark:text-surface-200">
      {children}
    </pre>
  )
}

/** The hosts and files a version carried, which are part of it as much as its prose. */
function Carried({
  hosts,
  files,
}: {
  hosts: string[]
  files?: { path: string; bytes: number }[]
}) {
  if (hosts.length === 0 && !files?.length) return null
  return (
    <dl className="space-y-1.5 text-xs">
      {hosts.length > 0 && (
        <div className="flex flex-wrap items-baseline gap-x-2">
          <dt className="inline-flex items-center gap-1 text-surface-600 dark:text-surface-400">
            <Globe size={11} aria-hidden />
            Hosts
          </dt>
          <dd className="font-mono text-surface-800 dark:text-surface-200">
            {hosts.join(', ')}
          </dd>
        </div>
      )}
      {files && files.length > 0 && (
        <div className="flex flex-wrap items-baseline gap-x-2">
          <dt className="inline-flex items-center gap-1 text-surface-600 dark:text-surface-400">
            <FileText size={11} aria-hidden />
            Files
          </dt>
          <dd className="font-mono text-surface-800 dark:text-surface-200">
            {files.map((f) => f.path).join(', ')}
          </dd>
        </div>
      )}
    </dl>
  )
}

function Diff({ sections }: { sections: ReturnType<typeof collapse> }) {
  return (
    <div className="max-h-96 overflow-auto rounded-md border border-surface-200 bg-surface-50 text-xs dark:border-surface-800 dark:bg-surface-950">
      {sections.map((section, at) =>
        section.kind === 'gap' ? (
          <p
            key={`gap-${at}`}
            className="border-y border-surface-200 bg-surface-100 px-3 py-1 text-center text-surface-500 dark:border-surface-800 dark:bg-surface-900 dark:text-surface-500"
          >
            {section.lines} unchanged {section.lines === 1 ? 'line' : 'lines'}
          </p>
        ) : (
          <div key={`run-${at}`}>
            {section.changes.map((change, i) => (
              <Line key={`${at}-${i}`} change={change} />
            ))}
          </div>
        ),
      )}
    </div>
  )
}

/**
 * One line of a diff.
 *
 * The sign carries the meaning rather than the colour alone, so this reads on a
 * monochrome display and to somebody who cannot tell the two greens from the two
 * reds -- which is most of why `+`/`-` survived into every diff since `ed`.
 *
 * It sits in a gutter of its own, tinted and ruled off from the text, because the
 * text is Markdown: a removed list item renders as `-` beside a `-`, and an
 * unchanged one begins with the same character the eye is scanning for. A sign
 * that shares a band with the prose is a sign the prose can imitate.
 */
function Line({ change }: { change: Change }) {
  const style =
    change.kind === 'added'
      ? 'bg-green-50 text-green-900 dark:bg-green-950/40 dark:text-green-200'
      : change.kind === 'removed'
        ? 'bg-red-50 text-red-900 dark:bg-red-950/40 dark:text-red-200'
        : 'text-surface-700 dark:text-surface-300'
  // Darker than the row, so the gutter reads as chrome rather than as the first
  // character of the line.
  const gutter =
    change.kind === 'added'
      ? 'bg-green-100 text-green-700 dark:bg-green-900/50 dark:text-green-300'
      : change.kind === 'removed'
        ? 'bg-red-100 text-red-700 dark:bg-red-900/50 dark:text-red-300'
        : 'text-surface-400 dark:text-surface-600'
  const sign = change.kind === 'added' ? '+' : change.kind === 'removed' ? '\u2212' : ' '

  return (
    <p className={`flex font-mono leading-relaxed ${style}`}>
      <span className="w-9 shrink-0 select-none py-0 pr-2 text-right text-surface-400 dark:text-surface-600">
        {change.line}
      </span>
      <span
        aria-hidden
        className={`w-5 shrink-0 select-none border-r text-center font-semibold border-surface-200 dark:border-surface-800 ${gutter}`}
      >
        {sign}
      </span>
      {/* The reader of a screen reader gets the word rather than the glyph: a
          bare "minus" read before every removed line is noise. */}
      <span className="sr-only">
        {change.kind === 'added' ? 'added' : change.kind === 'removed' ? 'removed' : ''}
      </span>
      <span className="whitespace-pre-wrap break-words pl-3 pr-3">{change.text || ' '}</span>
    </p>
  )
}

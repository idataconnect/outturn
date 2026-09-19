import type { ToolCallMessagePartComponent } from '@assistant-ui/react'
import ToolCall from './ToolCall'

/**
 * What a tool acted on, as it reported it once it answered.
 *
 * File tools name it `path`, the archive tools `archive`; unpacking also
 * names where the contents went, shown as `archive → destination`. A fetch
 * names its `url`, shown with its method.
 */
function reportedTarget(details: unknown): string {
  if (typeof details !== 'string' || details === '') return ''
  try {
    const parsed: unknown = JSON.parse(details)
    if (typeof parsed !== 'object' || parsed === null) return ''
    const { path, archive, destination, url, method } = parsed as Record<string, unknown>
    if (typeof url === 'string' && url !== '') {
      return typeof method === 'string' && method !== '' ? `${method} ${url}` : url
    }
    const file = typeof path === 'string' ? path : typeof archive === 'string' ? archive : ''
    if (file && typeof destination === 'string' && destination !== '') {
      return `${file} → ${destination}`
    }
    return file
  } catch {
    // Not JSON: nothing to name.
    return ''
  }
}

/**
 * A tool that acts on one thing names it. "Saving the summary" or "Looking
 * up the exchange rate" is what the model meant to do; the path or the URL is
 * what it actually did it to, and that is the part a reader would want to
 * check.
 */
const ToolTarget: ToolCallMessagePartComponent = (props) => {
  const target = reportedTarget(props.args?.details)
  return (
    <ToolCall {...props}>
      {target && (
        <p className="mt-1 break-all font-mono text-[11px] text-surface-700 dark:text-surface-300">
          {target}
        </p>
      )}
    </ToolCall>
  )
}

export default ToolTarget

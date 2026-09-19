import type { ToolCallMessagePartComponent } from '@assistant-ui/react'
import ToolCall from './ToolCall'

/**
 * Which tools a load actually made available.
 *
 * The agent names tools it wants and the guest hands back what it recognised,
 * so the interesting part is the list -- "Getting the tools ready" says nothing
 * about which ones, and whether the right ones arrived is the only question a
 * reader has about this call.
 *
 * A load that recognised nothing reports an error instead, which `ToolCall`
 * already draws; this adds nothing in that case rather than showing an empty
 * list beside a warning.
 */
function names(details: unknown): string {
  if (typeof details !== 'string' || details === '') return ''
  try {
    const parsed: unknown = JSON.parse(details)
    if (typeof parsed !== 'object' || parsed === null) return ''
    const { loaded } = parsed as Record<string, unknown>
    if (!Array.isArray(loaded)) return ''
    return loaded.filter((name): name is string => typeof name === 'string').join(', ')
  } catch {
    // Not JSON: nothing to list.
    return ''
  }
}

/**
 * A tool load, showing what it loaded.
 */
const ToolLoad: ToolCallMessagePartComponent = (props) => {
  const loaded = names(props.args?.details)
  return (
    <ToolCall {...props}>
      {loaded && (
        <p className="mt-1 break-all font-mono text-[11px] text-surface-700 dark:text-surface-300">
          {loaded}
        </p>
      )}
    </ToolCall>
  )
}

export default ToolLoad

import type { ToolCallMessagePartComponent } from '@assistant-ui/react'

/**
 * Tools that render as something other than their verb.
 *
 * Empty on purpose, and adding to it should be a decision rather than a
 * default. A tool not named here shows the verb the model wrote and nothing
 * more -- which is the whole of what most tool calls are worth to a reader,
 * and is never wrong in the way that showing raw output is wrong.
 *
 * A renderer earns its place by presenting something a person would actually
 * read: a file's contents in a viewer, a list as a list, a diff as a diff.
 * "The JSON, in a monospace box" is not that. If the answer to "what would
 * someone want to see here" is the tool's output verbatim, the honest move is
 * to leave the tool out of this map.
 *
 * Details still cross the wire for every tool. That is deliberate -- the data
 * is there the day someone writes a renderer for it -- but nothing reaches the
 * transcript unless something here asks for it.
 */
const toolRenderers: Record<string, ToolCallMessagePartComponent> = {}

export default toolRenderers

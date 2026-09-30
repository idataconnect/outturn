/**
 * Whether a record matches what somebody typed into a list's filter.
 *
 * Every word must appear somewhere in the record's text, in any order and any
 * case: "okafor loft" finds "Loft booking for Ms T. Okafor". An empty filter
 * matches everything.
 */
export function matchesFilter(query: string, ...fields: (string | null | undefined)[]): boolean {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean)
  if (words.length === 0) return true
  const text = fields.filter(Boolean).join(' ').toLowerCase()
  return words.every((w) => text.includes(w))
}

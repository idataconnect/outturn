import { describe, expect, it } from 'vitest'

import { declaresApproval, mentionedIn, pathProblem } from './skills'

describe('a file the instructions never mention', () => {
  const body = 'Read `skill/hollowbrook/get_room.md` before describing a room.'

  it('is found by its full path', () => {
    expect(mentionedIn(body, 'hollowbrook', 'get_room.md')).toBe(true)
  })

  it('or by its bare name', () => {
    expect(mentionedIn('See create_booking.md.', 'hollowbrook', 'create_booking.md')).toBe(true)
  })

  /// The case the warning exists for: files are never sent, so one the
  /// instructions do not name is one the agent will not know is there.
  it('is reported as not mentioned', () => {
    expect(mentionedIn(body, 'hollowbrook', 'list_rooms.md')).toBe(false)
  })
})

describe("a file's path", () => {
  it('accepts an ordinary nested path', () => {
    expect(pathProblem('ops/get_room.md', [])).toBeNull()
  })

  it.each([
    ['', 'needs a name'],
    ['/abs.md', 'must not start with /'],
    ['a/../b.md', 'has an empty, . or .. part'],
    ['a//b.md', 'has an empty, . or .. part'],
    ['back\\slash.md', 'contains a backslash or a control character'],
  ])('refuses %j the way the API would', (path, why) => {
    expect(pathProblem(path, [])).toBe(why)
  })

  it('refuses a name another file already has', () => {
    expect(pathProblem('get_room.md', ['get_room.md'])).toBe('is already a file here')
  })
})

describe('a file that declares an approval', () => {
  it('is recognised from its frontmatter', () => {
    expect(declaresApproval('---\napproval:\n  requires: charge\n---\n# charge')).toBe(true)
  })

  it('is not confused by the word in the prose', () => {
    expect(declaresApproval('# charge\n\napproval: happens elsewhere')).toBe(false)
  })

  it('is not confused by other frontmatter', () => {
    expect(declaresApproval('---\ntitle: x\n---\n')).toBe(false)
  })
})

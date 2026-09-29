import { describe, expect, it } from 'vitest'

import { declaresApproval, fileChanges, mentionedIn, pathProblem } from './skills'

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

describe('which files a version changed', () => {
  const f = (path: string, sha256: string) => ({ path, sha256 })

  /// The case the history could not show: one operation's file edited and
  /// nothing else, which listed as the same names as the version before.
  it('names the one file that was edited', () => {
    expect(
      fileChanges(
        [f('charge.md', 'a'), f('rooms.md', 'b')],
        [f('charge.md', 'c'), f('rooms.md', 'b')],
      ),
    ).toEqual([{ path: 'charge.md', change: 'changed' }])
  })

  it('tells an added file from a removed one', () => {
    expect(fileChanges([f('old.md', 'a')], [f('new.md', 'b')])).toEqual([
      { path: 'new.md', change: 'added' },
      { path: 'old.md', change: 'removed' },
    ])
  })

  it('says nothing when the files are the same', () => {
    expect(fileChanges([f('a.md', 'x')], [f('a.md', 'x')])).toEqual([])
  })
})

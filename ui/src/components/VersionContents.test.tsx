import { fireEvent, render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'

import VersionContents from './VersionContents'

const version = (over: Partial<Parameters<typeof VersionContents>[0]['version']> = {}) => ({
  body: 'Hollowbrook House is a guesthouse.\nYou can read its rooms.',
  hosts: ['outturn-hollowbrook:8084'],
  files: [{ path: 'charge.md', bytes: 400 }],
  ...over,
})

describe('what a version said', () => {
  it('shows nothing until asked', () => {
    // Twenty versions each rendering a page of prose is a history nobody can scan.
    render(<VersionContents version={version()} live="something else" isLive={false} />)
    expect(screen.queryByText(/Hollowbrook House is a guesthouse/)).not.toBeInTheDocument()
  })

  it('shows the body when opened', async () => {
    render(<VersionContents version={version()} live="something else" isLive={false} />)
    await userEvent.click(screen.getByRole('button', { name: /What it said/ }))
    expect(screen.getByText(/Hollowbrook House is a guesthouse/)).toBeInTheDocument()
  })

  it('shows the hosts and files a version carried', async () => {
    // Part of the version as much as its prose: restoring brings them back too.
    render(<VersionContents version={version()} live="x" isLive={false} />)
    await userEvent.click(screen.getByRole('button', { name: /What it said/ }))
    expect(screen.getByText('outturn-hollowbrook:8084')).toBeInTheDocument()
    expect(screen.getByText('charge.md')).toBeInTheDocument()
  })

  it('closes again', async () => {
    render(<VersionContents version={version()} live="x" isLive={false} />)
    const open = screen.getByRole('button', { name: /What it said/ })
    await userEvent.click(open)
    await userEvent.click(screen.getByRole('button', { name: /Hide what it said/ }))
    expect(screen.queryByText(/Hollowbrook House is a guesthouse/)).not.toBeInTheDocument()
  })
})

describe('a version the history only summarised', () => {
  /// The history lists versions without their prose; a version is read when
  /// somebody opens it, and not before.
  it('is read when opened, and not before', async () => {
    const load = vi.fn().mockResolvedValue({ body: 'What v3 said.', files: [] })
    render(
      <VersionContents
        version={{ id: 'v3', hosts: [], changed: [{ path: 'a.md', change: 'changed' }] }}
        load={load}
        live="x"
        isLive={false}
      />,
    )
    expect(load).not.toHaveBeenCalled()
    // What it changed comes from the summary, with nothing read.
    expect(screen.getByText('a.md')).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: /What it said/ }))
    expect(await screen.findByText('What v3 said.')).toBeInTheDocument()
    expect(load).toHaveBeenCalledTimes(1)
  })
})

describe('comparing with the live version', () => {
  it('offers no comparison on the live version itself', () => {
    // An empty diff of the live version against itself explains nothing.
    render(<VersionContents version={version()} live={version().body} isLive />)
    expect(screen.queryByRole('button', { name: /Compare/ })).not.toBeInTheDocument()
  })

  it('shows what restoring would change', async () => {
    render(
      <VersionContents
        version={version({ body: 'one\nold line\nthree' })}
        live={'one\nnew line\nthree'}
        isLive={false}
      />,
    )
    await userEvent.click(screen.getByRole('button', { name: /Compare with live/ }))
    // The old text and the new, both on screen, so the reader sees the swap.
    expect(screen.getByText('old line')).toBeInTheDocument()
    expect(screen.getByText('new line')).toBeInTheDocument()
    expect(screen.getAllByText('removed').length).toBeGreaterThan(0)
    expect(screen.getAllByText('added').length).toBeGreaterThan(0)
  })

  it('says so when the words are the same', async () => {
    // Two versions can differ in their note, hosts or files and say the same
    // thing; an empty panel would read as a broken diff.
    render(<VersionContents version={version()} live={version().body} isLive={false} />)
    await userEvent.click(screen.getByRole('button', { name: /Compare with live/ }))
    expect(screen.getByText(/words are the same as the live version/)).toBeInTheDocument()
  })

  it('collapses a long unchanged run rather than printing it', async () => {
    const lines = Array.from({ length: 40 }, (_, i) => `line ${i}`).join('\n')
    render(
      <VersionContents
        version={version({ body: lines })}
        live={lines.replace('line 20', 'line 20 edited')}
        isLive={false}
      />,
    )
    await userEvent.click(screen.getByRole('button', { name: /Compare with live/ }))
    // Two gaps, one either side of the change: the collapse keeps context around
    // what moved and folds the rest.
    expect(screen.getAllByText(/unchanged lines/)).toHaveLength(2)
    expect(screen.queryByText('line 0')).not.toBeInTheDocument()
    expect(screen.getByText('line 20 edited')).toBeInTheDocument()
  })

  it('refuses a body too long to compare rather than freezing', async () => {
    const huge = Array.from({ length: 2001 }, (_, i) => `line ${i}`).join('\n')
    render(<VersionContents version={version({ body: huge })} live="one" isLive={false} />)
    await userEvent.click(screen.getByRole('button', { name: /Compare with live/ }))
    expect(screen.getByText(/Too long to compare/)).toBeInTheDocument()
  })

  it('offers no comparison when there is no live body to compare against', () => {
    render(<VersionContents version={version()} isLive={false} />)
    expect(screen.queryByRole('button', { name: /Compare/ })).not.toBeInTheDocument()
    // Reading it is still possible, which is the half that needs nothing else.
    expect(screen.getByRole('button', { name: /What it said/ })).toBeInTheDocument()
  })
})

describe('a version with nothing beside its prose', () => {
  it('shows no hosts or files row', async () => {
    render(
      <VersionContents
        version={{ body: 'Just prose.', hosts: [], files: [] }}
        live="x"
        isLive={false}
      />,
    )
    await userEvent.click(screen.getByRole('button', { name: /What it said/ }))
    expect(screen.getByText('Just prose.')).toBeInTheDocument()
    expect(screen.queryByText('Hosts')).not.toBeInTheDocument()
    expect(screen.queryByText('Files')).not.toBeInTheDocument()
  })
})

describe('which version is compared against', () => {
  /// The direction is the whole point and nothing asserted it: swapping the basis
  /// left all 221 tests passing while the diff answered a different question.
  ///
  /// "Should I restore this?" means "what would be different afterwards", so the
  /// comparison is against the live version. Against the one before it, the diff
  /// would describe a change somebody already made and moved on from.
  it('reads as the change restoring would make, not the change this version made', async () => {
    // v1 said "old", v2 (live) says "new", and we are looking at v1.
    render(
      <VersionContents
        version={version({ body: 'greeting\nold wording\nend' })}
        live={'greeting\nnew wording\nend'}
        isLive={false}
      />,
    )
    await userEvent.click(screen.getByRole('button', { name: /Compare with live/ }))

    // Restoring v1 would put "old wording" back and take "new wording" away, so
    // the old text is the addition and the live text the removal.
    const added = screen.getByText('old wording').closest('p')
    const removed = screen.getByText('new wording').closest('p')
    expect(added?.textContent).toContain('added')
    expect(removed?.textContent).toContain('removed')
  })
})

describe('the files a version changed', () => {
  const file = (path: string, sha256: string) => ({ path, sha256, bytes: 10 })

  /// The gap this closes: an edit to one file and nothing else listed under
  /// the same names as the version before it, so the history could not say
  /// which file had changed.
  it('names the file this version changed, without opening anything', () => {
    render(
      <VersionContents
        version={version({ files: [file('charge.md', 'new'), file('rooms.md', 'same')] })}
        isLive
        previousFiles={[file('charge.md', 'old'), file('rooms.md', 'same')]}
      />,
    )
    expect(screen.getByText('changed')).toBeInTheDocument()
    expect(screen.getByText('charge.md')).toBeInTheDocument()
    expect(screen.queryByText('rooms.md')).not.toBeInTheDocument()
  })

  it('opens a changed file to its own diff against the live version', async () => {
    const read = vi.fn(async (versionId: string) =>
      versionId === 'live' ? 'take the money\n' : 'ask first\ntake the money\n',
    )
    render(
      <VersionContents
        version={version({ id: 'old', files: [file('charge.md', 'a')] })}
        live={version().body}
        isLive={false}
        liveFiles={[file('charge.md', 'b')]}
        liveVersionId="live"
        readFile={read}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: /Compare with live/ }))
    fireEvent.click(await screen.findByRole('button', { name: /charge\.md/ }))
    expect(await screen.findByText('ask first')).toBeInTheDocument()
    expect(read).toHaveBeenCalledWith('live', 'charge.md')
    expect(read).toHaveBeenCalledWith('old', 'charge.md')
  })

  /// Contents are read only when a file is opened; listing what changed needs
  /// the hashes alone, and a version can carry hundreds of files.
  it('reads nothing until a file is opened', () => {
    const read = vi.fn(async () => '')
    render(
      <VersionContents
        version={version({ id: 'old', files: [file('a.md', 'x')] })}
        live="other"
        isLive={false}
        liveFiles={[file('a.md', 'y')]}
        liveVersionId="live"
        readFile={read}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: /Compare with live/ }))
    expect(read).not.toHaveBeenCalled()
  })

  it('says so when the files match the live version', () => {
    render(
      <VersionContents
        version={version({ files: [file('a.md', 'x')] })}
        live="other"
        isLive={false}
        liveFiles={[file('a.md', 'x')]}
        liveVersionId="live"
        readFile={async () => ''}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: /Compare with live/ }))
    expect(screen.getByText(/same as the live version/)).toBeInTheDocument()
  })
})

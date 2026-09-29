import { fireEvent, render, screen } from '@testing-library/react'
import { useState } from 'react'
import { describe, expect, it } from 'vitest'

import SkillFiles from './SkillFiles'
import type { DraftFile } from '../lib/skills'

function Harness({ start, body }: { start: DraftFile[]; body: string }) {
  const [files, setFiles] = useState(start)
  return (
    <>
      <SkillFiles files={files} onChange={setFiles} body={body} slug="house" editable />
      <output data-testid="paths">{files.map((f) => f.path).join(',')}</output>
    </>
  )
}

const paths = () => screen.getByTestId('paths').textContent

describe('the files a skill carries', () => {
  it('says plainly when there are none', () => {
    render(<Harness start={[]} body="" />)
    expect(screen.getByText(/No files/)).toBeInTheDocument()
  })

  /// The warning the section is built around: files are never sent, so one the
  /// instructions do not name is one the agent will not know is there.
  it('warns about a file the instructions never mention', () => {
    render(<Harness start={[{ path: 'refunds.md', content: 'how' }]} body="Nothing here." />)
    expect(screen.getByText(/never mention this file/)).toBeInTheDocument()
    expect(screen.getByText('skill/house/refunds.md')).toBeInTheDocument()
  })

  it('says nothing about a file the instructions point to', () => {
    render(
      <Harness
        start={[{ path: 'refunds.md', content: 'how' }]}
        body="For refunds read skill/house/refunds.md."
      />,
    )
    expect(screen.queryByText(/never mention this file/)).not.toBeInTheDocument()
  })

  it('adds a file and removes it again', () => {
    render(<Harness start={[]} body="" />)
    fireEvent.click(screen.getByRole('button', { name: /New file/ }))
    expect(paths()).toBe('operation-1.md')
    fireEvent.click(screen.getByRole('button', { name: /Remove this file/ }))
    expect(paths()).toBe('')
  })

  it('renames a file when the name is left', () => {
    render(<Harness start={[{ path: 'a.md', content: '' }]} body="" />)
    const name = screen.getByLabelText('File name')
    fireEvent.change(name, { target: { value: 'ops/get_room.md' } })
    fireEvent.blur(name)
    expect(paths()).toBe('ops/get_room.md')
  })

  /// Two files cannot share a path, and a rename that would make them is kept
  /// out rather than merging them.
  it('refuses a rename onto another file', () => {
    render(
      <Harness
        start={[
          { path: 'a.md', content: '' },
          { path: 'b.md', content: '' },
        ]}
        body=""
      />,
    )
    const name = screen.getByLabelText('File name')
    fireEvent.change(name, { target: { value: 'b.md' } })
    expect(screen.getByRole('alert')).toHaveTextContent('already a file here')
    fireEvent.blur(name)
    expect(paths()).toBe('a.md,b.md')
  })

  it('marks a file that declares an approval', () => {
    render(
      <Harness
        start={[{ path: 'charge.md', content: '---\napproval:\n  requires: charge\n---\n' }]}
        body="charge.md"
      />,
    )
    expect(screen.getByRole('img', { name: 'needs approval' })).toBeInTheDocument()
  })
})

import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { FileText, Info } from 'lucide-react'

import SidePane, { type PaneTab } from './SidePane'

function setWidth(px: number) {
  // jsdom has no layout, so matchMedia is the only thing the components read.
  vi.stubGlobal('matchMedia', (query: string) => {
    const min = Number(/min-width:\s*(\d+)px/.exec(query)?.[1] ?? 0)
    return {
      matches: px >= min,
      media: query,
      addEventListener: () => {},
      removeEventListener: () => {},
    }
  })
}

const TABS: PaneTab[] = [
  { id: 'files', label: 'Files', icon: FileText, render: () => <p>file list</p> },
  { id: 'about', label: 'Overview', icon: Info, render: () => <p>overview body</p> },
]

function show(tabs = TABS) {
  return render(<SidePane tabs={tabs} storageKey="test.pane" />)
}

describe('SidePane', () => {
  beforeEach(() => {
    localStorage.clear()
    setWidth(1440)
  })

  it('shows the rail even while the panel is shut', async () => {
    const user = userEvent.setup()
    show()
    // A desktop starts open, so the first click on the showing tab shuts it.
    expect(screen.getByText('file list')).toBeInTheDocument()

    // Closing leaves the icons behind: a tab nobody can see is a tab nobody opens.
    await user.click(screen.getByRole('tab', { name: 'Files' }))
    expect(screen.queryByText('file list')).not.toBeInTheDocument()
    expect(screen.getByRole('tab', { name: 'Files' })).toBeInTheDocument()
    expect(screen.getByRole('tab', { name: 'Overview' })).toBeInTheDocument()

    // And the same icon brings it back.
    await user.click(screen.getByRole('tab', { name: 'Files' }))
    expect(screen.getByText('file list')).toBeInTheDocument()
  })

  it('switches tabs without closing, and only closes on the open one', async () => {
    const user = userEvent.setup()
    show()
    await user.click(screen.getByRole('tab', { name: 'Overview' }))

    expect(screen.getByText('overview body')).toBeInTheDocument()
    expect(screen.queryByText('file list')).not.toBeInTheDocument()
  })

  it('renders only the active tab, so a closed tab does no work', async () => {
    const user = userEvent.setup()
    const render1 = vi.fn(() => <p>one</p>)
    const render2 = vi.fn(() => <p>two</p>)
    show([
      { id: 'a', label: 'A', icon: FileText, render: render1 },
      { id: 'b', label: 'B', icon: Info, render: render2 },
    ])

    // A is open by default on a desktop; B has never been asked for.
    expect(render1).toHaveBeenCalled()
    expect(render2).not.toHaveBeenCalled()

    await user.click(screen.getByRole('tab', { name: 'B' }))
    expect(render2).toHaveBeenCalled()
  })

  it('remembers the tab and the open state across a remount', async () => {
    const user = userEvent.setup()
    const first = show()
    await user.click(screen.getByRole('tab', { name: 'Overview' }))
    first.unmount()

    show()
    await waitFor(() => expect(screen.getByText('overview body')).toBeInTheDocument())
  })

  it('starts shut on a phone and open on a desktop', () => {
    setWidth(390)
    const phone = show()
    expect(screen.queryByText('file list')).not.toBeInTheDocument()
    phone.unmount()

    localStorage.clear()
    setWidth(1440)
    show()
    expect(screen.getByText('file list')).toBeInTheDocument()
  })

  it('drops a tab the viewer may not have', () => {
    show([
      TABS[0],
      { ...TABS[1], available: false },
    ])
    expect(screen.getByRole('tab', { name: 'Files' })).toBeInTheDocument()
    expect(screen.queryByRole('tab', { name: 'Overview' })).not.toBeInTheDocument()
  })

  it('closes on Escape while floating over the thread', async () => {
    const user = userEvent.setup()
    setWidth(800) // tablet: the panel floats
    show()
    await user.click(screen.getByRole('tab', { name: 'Files' }))
    expect(screen.getByText('file list')).toBeInTheDocument()

    await user.keyboard('{Escape}')
    expect(screen.queryByText('file list')).not.toBeInTheDocument()
  })
})

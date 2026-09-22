import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import {
  AssistantRuntimeProvider,
  useExternalStoreRuntime,
  type ThreadMessageLike,
} from '@assistant-ui/react'
import { describe, expect, it } from 'vitest'

import Thread from './Thread'
import type { SkillCommand } from '../lib/useSkillCommands'

function Harness({ skills }: { skills?: SkillCommand[] }) {
  const runtime = useExternalStoreRuntime<ThreadMessageLike>({
    messages: [],
    convertMessage: (message) => message,
    onNew: async () => {},
    onCancel: async () => {},
    queue: {
      items: [],
      steerItems: [],
      enqueue: () => {},
      steer: () => {},
      move: () => {},
      edit: () => {},
      remove: () => {},
    },
  })
  return (
    <AssistantRuntimeProvider runtime={runtime}>
      <Thread sessionId="s1" skills={skills} />
    </AssistantRuntimeProvider>
  )
}

const composer = () => screen.getByPlaceholderText('Message the agent…')

// Slugs and names that differ, because that is where writing the wrong one
// shows: a skill is `## Customer Onboarding` in the turn's prompt and
// `customer-onboarding` in the database, and the model only ever sees the
// first.
const onboarding: SkillCommand[] = [
  { id: 'customer-onboarding', label: 'Customer Onboarding', description: 'Walks the steps' },
  { id: 'raise-invoice', label: 'Raise an Invoice', description: 'From a purchase order' },
]

describe('the skill menu', () => {
  it('offers the agent its skills when a slash is typed', async () => {
    const user = userEvent.setup()
    render(<Harness skills={onboarding} />)

    await user.click(composer())
    await user.keyboard('/')

    expect(await screen.findByText('Customer Onboarding')).toBeInTheDocument()
    expect(screen.getByText('Raise an Invoice')).toBeInTheDocument()
  })

  it('narrows to what is still being typed', async () => {
    const user = userEvent.setup()
    render(<Harness skills={onboarding} />)

    await user.click(composer())
    await user.keyboard('/inv')

    expect(await screen.findByText('Raise an Invoice')).toBeInTheDocument()
    expect(screen.queryByText('Customer Onboarding')).not.toBeInTheDocument()
  })

  it('writes words a model can read, not directive syntax', async () => {
    const user = userEvent.setup()
    render(<Harness skills={onboarding} />)

    await user.click(composer())
    await user.keyboard('/inv')
    await user.click(await screen.findByText('Raise an Invoice'))

    // Two things at once. The library's default writes
    // `:command[Raise an Invoice]{name=raise-invoice}`, which nothing here
    // renders -- so the model got it raw, said it was not a syntax it
    // responded to, and then made something up rather than stopping.
    //
    // And the name, not the slug: src/api/skill/mod.rs gives each skill a
    // `## {name}` heading in the turn's prompt, and the slug reaches the model
    // nowhere at all.
    const written = (composer() as HTMLTextAreaElement).value
    expect(written).toContain('run skill Raise an Invoice')
    expect(written).not.toContain('raise-invoice')
    expect(written).not.toContain(':command[')
  })

  it('stays out of the way when the agent has no skills', async () => {
    const user = userEvent.setup()
    render(<Harness skills={[]} />)

    await user.click(composer())
    await user.keyboard('/')

    // An empty popover on every slash would punish anybody typing a path or
    // a fraction, which is most of what a slash is for.
    expect(screen.queryByRole('button', { name: /onboarding/i })).not.toBeInTheDocument()
  })
})

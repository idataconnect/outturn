import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import {
  AssistantRuntimeProvider,
  useExternalStoreRuntime,
  type AppendMessage,
  type ThreadMessageLike,
} from '@assistant-ui/react'
import { describe, expect, it } from 'vitest'

import Thread from './Thread'
import type { SkillCommand } from '../lib/useSkillCommands'

function Harness({
  skills,
  messages = [],
  sent = [],
}: {
  skills?: SkillCommand[]
  messages?: ThreadMessageLike[]
  /** Filled with what the composer hands the runtime. */
  sent?: AppendMessage[]
}) {
  const runtime = useExternalStoreRuntime<ThreadMessageLike>({
    messages,
    convertMessage: (message) => message,
    onNew: async () => {},
    onCancel: async () => {},
    queue: {
      items: [],
      steerItems: [],
      enqueue: (message) => {
        sent.push(message)
      },
      steer: (message) => {
        sent.push(message)
      },
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

const composer = () => screen.getByPlaceholderText('Message the agent…') as HTMLTextAreaElement

// Slugs and names that differ, because that is where sending the wrong one
// shows: the API looks a skill up by its slug, and a person reads its name.
const onboarding: SkillCommand[] = [
  { id: 'customer-onboarding', label: 'Customer Onboarding', description: 'Walks the steps' },
  { id: 'raise-invoice', label: 'Raise an Invoice', description: 'From a purchase order' },
]

const skillOf = (message: AppendMessage) =>
  (message.runConfig?.custom as { skill?: { slug: string; name: string } } | undefined)?.skill

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

  it('puts the skill in a chip rather than in the message', async () => {
    const user = userEvent.setup()
    render(<Harness skills={onboarding} />)

    await user.click(composer())
    await user.keyboard('/inv')
    await user.click(await screen.findByText('Raise an Invoice'))

    // Nothing written: the earlier attempts wrote directive syntax the model
    // could not read, and then `run skill <Name>`, which reached it as the
    // person's own sentence.
    expect(composer().value.trim()).toBe('')
    expect(screen.getByRole('button', { name: 'Remove the skill Raise an Invoice' })).toBeInTheDocument()
    expect(document.body.textContent).not.toContain('run skill')
  })

  it('takes the chip away again', async () => {
    const user = userEvent.setup()
    render(<Harness skills={onboarding} />)

    await user.click(composer())
    await user.keyboard('/inv')
    await user.click(await screen.findByText('Raise an Invoice'))
    await user.click(screen.getByRole('button', { name: 'Remove the skill Raise an Invoice' }))

    expect(screen.queryByText('Raise an Invoice')).not.toBeInTheDocument()
  })

  it('sends the slug beside what was typed', async () => {
    const user = userEvent.setup()
    const sent: AppendMessage[] = []
    render(<Harness skills={onboarding} sent={sent} />)

    await user.click(composer())
    await user.keyboard('/inv')
    await user.click(await screen.findByText('Raise an Invoice'))
    await user.click(composer())
    await user.keyboard('for PO 1182')
    await user.click(screen.getByRole('button', { name: 'Send' }))

    await waitFor(() => expect(sent).toHaveLength(1))
    expect(skillOf(sent[0])).toEqual({ slug: 'raise-invoice', name: 'Raise an Invoice' })
    // Trimmed by the API, as every message is; the menu leaves a space where
    // the slash was.
    const [part] = sent[0].content
    expect(part.type === 'text' && part.text.trim()).toBe('for PO 1182')
    // Sent with that message and not the next.
    await waitFor(() =>
      expect(screen.queryByRole('button', { name: /Remove the skill/ })).not.toBeInTheDocument(),
    )
  })

  it('sends a skill picked with nothing typed', async () => {
    const user = userEvent.setup()
    const sent: AppendMessage[] = []
    render(<Harness skills={onboarding} sent={sent} />)

    await user.click(composer())
    await user.keyboard('/inv')
    await user.click(await screen.findByText('Raise an Invoice'))
    await user.click(screen.getByRole('button', { name: 'Send' }))

    await waitFor(() => expect(sent).toHaveLength(1))
    expect(skillOf(sent[0])).toEqual({ slug: 'raise-invoice', name: 'Raise an Invoice' })
    expect(sent[0].content).toEqual([])
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

describe('a message that invoked a skill', () => {
  const invoking = (text: string): ThreadMessageLike => ({
    id: 'm1',
    role: 'user',
    content: [{ type: 'text', text }],
    metadata: { custom: { skill: { slug: 'raise-invoice', name: 'Raise an Invoice' } } },
  })

  it('shows the skill as a chip beside what was typed', () => {
    render(<Harness messages={[invoking('for PO 1182')]} />)

    expect(screen.getByText('Raise an Invoice')).toBeInTheDocument()
    expect(screen.getByText('Skill:')).toBeInTheDocument()
    expect(screen.getByText('for PO 1182')).toBeInTheDocument()
    expect(document.body.textContent).not.toContain('run skill')
  })

  it('is the chip alone when nothing was typed', () => {
    const { container } = render(<Harness messages={[invoking('')]} />)

    expect(screen.getByText('Raise an Invoice')).toBeInTheDocument()
    const bubble = container.querySelector('.user')
    expect(bubble?.textContent).toBe('Skill: Raise an Invoice')
  })
})

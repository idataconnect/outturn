import type { ReactNode } from 'react'

import {
  ComposerPrimitive,
  unstable_useSlashCommandAdapter,
  type Unstable_DirectiveFormatter,
} from '@assistant-ui/react'

import type { SkillCommand } from '../lib/useSkillCommands'

/**
 * The `/` menu over the composer, listing the skills this agent has.
 *
 * Picking one writes its slug into the message. Not a protocol: the agent
 * decides what to use, as it always has, and this only saves somebody
 * remembering what a skill was called and typing it exactly. A menu that
 * bound the turn to one skill would be a different feature with a different
 * risk -- it would have to be honoured somewhere, and nothing honours it yet.
 *
 * Built on the trigger primitives rather than the styled component their
 * example names: `ComposerTriggerPopover` lives in @assistant-ui/react-ui,
 * which this project does not use for the reason the Thread's own comment
 * gives. The primitives underneath do the detection, the filtering, the
 * keyboard navigation and the dismissal; what is here is markup.
 *
 * Wraps the composer rather than sitting inside it. The root is a provider
 * that watches what is typed, so the input has to be within it -- and the
 * first attempt, with the popover nested in the composer, threw
 * "useTriggerPopoverRootContext must be used within
 * ComposerPrimitive.TriggerPopoverRoot" on the first keystroke.
 *
 * Marked unstable by the library, and it is one hook and one file, so a
 * breaking change on a deliberate upgrade is a compile error here and
 * nowhere else.
 */
/**
 * What picking a skill puts in the message.
 *
 * The library's default writes `:command[Randomness]{name=randomness}` --
 * markdown-directive syntax, meant for a renderer to turn into a visual pill.
 * Nothing renders it here, so the model received it raw and had to guess: one
 * agent answered that this was not a command syntax it responded to, and then
 * invented a fact rather than stopping. That is the failure this avoids.
 *
 * Plain words instead, legible to the model, to a person reading the
 * transcript back, and to anything else that ever reads a message.
 *
 * `parse` hands the text back whole rather than recognising what `serialize`
 * wrote. The round trip exists so a renderer can find directives to draw; a
 * sentence has none to find, and claiming otherwise would draw a pill around
 * three ordinary words.
 */
const plainWords: Unstable_DirectiveFormatter = {
  // The name, not the id. The slug is a database and URL identifier and
  // reaches the model nowhere: src/api/skill/mod.rs composes the turn's prompt
  // with `## {name}` headings and `ResolvedSkill` has no slug field to write
  // even if it wanted to. So a message saying `run skill customer-onboarding`
  // asks the model to match a slug against a heading called "Customer
  // Onboarding" -- near enough to work by luck, and wrong the moment the two
  // drift, which a rename does permanently because the slug does not follow.
  serialize: (item) => `run skill ${item.label ?? item.id}`,
  parse: (text) => [{ kind: 'text', text }],
}

export default function SkillMenu({
  commands,
  children,
}: {
  commands: SkillCommand[];
  children: ReactNode;
}) {
  const slash = unstable_useSlashCommandAdapter({
    // Selecting writes the slug and nothing else. `execute` is where a
    // version that did something to the turn would put it, and deliberately
    // does nothing.
    commands: commands.map((command) => ({
      id: command.id,
      label: command.label,
      description: command.description,
      execute: () => {},
    })),
  });

  // Nothing to offer: an empty popover on every `/` would punish anybody
  // typing a path or a fraction, which is most of what a slash is for.
  if (commands.length === 0) return <>{children}</>;

  return (
    <ComposerPrimitive.Unstable_TriggerPopoverRoot>
      <ComposerPrimitive.Unstable_TriggerPopover
        char='/'
        adapter={slash.adapter}
        className='z-40 w-80 max-h-64 overflow-auto rounded-md border border-surface-200 dark:border-surface-700 bg-white dark:bg-surface-800 shadow-lg p-1'
      >
        {/* Left behind rather than stripped: what somebody picked is part of
            what they sent, and a menu that erased its own trace would leave a
            message nobody could read back. Written as words, not as a
            directive -- see `plainWords`. */}
        <ComposerPrimitive.Unstable_TriggerPopover.Action
          formatter={plainWords}
          onExecute={slash.action.onExecute}
        />
        <ComposerPrimitive.Unstable_TriggerPopoverItems>
          {(items) =>
            items.map((item, index) => (
              <ComposerPrimitive.Unstable_TriggerPopoverItem
                key={item.id}
                item={item}
                index={index}
                className='w-full text-left px-2 py-1.5 rounded text-sm text-surface-700 dark:text-surface-200 data-[highlighted]:bg-surface-100 dark:data-[highlighted]:bg-surface-700'
              >
                <span className="block font-medium truncate">
                  {item.label ?? item.id}
                </span>
                {item.description && (
                  <span className="block text-xs text-surface-500 dark:text-surface-400 truncate">
                    {item.description}
                  </span>
                )}
              </ComposerPrimitive.Unstable_TriggerPopoverItem>
            ))
          }
        </ComposerPrimitive.Unstable_TriggerPopoverItems>
      </ComposerPrimitive.Unstable_TriggerPopover>
      {children}
    </ComposerPrimitive.Unstable_TriggerPopoverRoot>
  );
}

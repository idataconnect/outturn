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
 * Picking one invokes it: the composer shows the skill as a chip beside what
 * is typed, and the message carries the skill's slug alongside its content
 * rather than inside it. The API checks the agent has it, keeps the pick on
 * the stored message, and tells the turn answering it what was picked -- see
 * `skill::invoked` in the API. So nothing is written into the message: the
 * two earlier attempts wrote markdown-directive syntax, which the model read
 * raw and could not make sense of, and then the words `run skill <Name>`,
 * which reached the model as the person's own sentence and showed in the
 * transcript as something they had typed.
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
 * What picking anything puts in the message: nothing. The slash and what was
 * typed after it go; a skill becomes the chip, an action runs.
 *
 * `parse` hands the text back whole, since nothing is written for it to find.
 */
const nothing: Unstable_DirectiveFormatter = {
  serialize: () => '',
  parse: (text) => [{ kind: 'text', text }],
}

/** Something the menu does rather than writes: picking it runs `run` and
 *  leaves nothing in the message. */
export type MenuAction = {
  id: string;
  label: string;
  description: string;
  run: () => void;
};

export default function SkillMenu({
  commands,
  actions = [],
  onInvoke,
  children,
}: {
  commands: SkillCommand[];
  actions?: MenuAction[];
  /** Told which skill was picked, for the composer to show and send. */
  onInvoke?: (command: SkillCommand) => void;
  children: ReactNode;
}) {
  const slash = unstable_useSlashCommandAdapter({
    commands: [
      ...actions.map((action) => ({
        id: action.id,
        label: action.label,
        description: action.description,
        execute: action.run,
      })),
      ...commands.map((command) => ({
        id: command.id,
        label: command.label,
        description: command.description,
        execute: () => onInvoke?.(command),
      })),
    ],
  });

  // Nothing to offer: an empty popover on every `/` would punish anybody
  // typing a path or a fraction, which is most of what a slash is for.
  if (commands.length === 0 && actions.length === 0) return <>{children}</>;

  return (
    <ComposerPrimitive.Unstable_TriggerPopoverRoot>
      <ComposerPrimitive.Unstable_TriggerPopover
        char='/'
        adapter={slash.adapter}
        className='z-40 w-80 max-h-64 overflow-auto rounded-md border border-surface-200 dark:border-surface-700 bg-white dark:bg-surface-800 shadow-lg p-1'
      >
        <ComposerPrimitive.Unstable_TriggerPopover.Action
          formatter={nothing}
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

import { useEffect, useState } from 'react'

import { agentSkills, listSkills, type Skill } from './skills'

/**
 * The skills an agent actually has, for the composer's `/` menu.
 *
 * Bindings carry ids and nothing else, so the names come from the workspace's
 * skill list and the two are joined here. Only what this agent is bound to:
 * offering the whole workspace would put things in the menu that picking does
 * nothing about, since a skill the agent has not been given is not one it can
 * use.
 *
 * Retired skills are left out for the same reason. A binding to one survives
 * -- retiring is not unbinding -- but it is not something to suggest.
 */
export type SkillCommand = {
  /** The slug, which is what `/` matches on and what gets typed. */
  id: string
  label: string
  description: string
}

export function useSkillCommands(agentId: string | null): SkillCommand[] {
  const [commands, setCommands] = useState<SkillCommand[]>([])

  useEffect(() => {
    if (!agentId) {
      setCommands([])
      return
    }
    let stopped = false
    void (async () => {
      try {
        const [all, bound] = await Promise.all([listSkills(), agentSkills(agentId)])
        if (stopped) return
        const byId = new Map<string, Skill>(all.map((s) => [s.id, s]))
        setCommands(
          bound
            // In the order the agent lists them, which is the order it was
            // given and the order somebody arranging them chose.
            .sort((a, b) => a.position - b.position)
            .map((binding) => byId.get(binding.skill_id))
            .filter((skill): skill is Skill => !!skill && !skill.retired_at)
            .map((skill) => ({
              id: skill.slug,
              label: skill.name,
              description: skill.description,
            })),
        )
      } catch {
        // A menu is a convenience. Failing to load one is not worth a banner
        // over the conversation: the composer works, and typing still does
        // everything it did before.
        if (!stopped) setCommands([])
      }
    })()
    return () => {
      stopped = true
    }
  }, [agentId])

  return commands
}

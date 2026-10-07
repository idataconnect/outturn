/**
 * Who did something, as the API says this workspace may be told.
 *
 * The rules live in the API (`src/api/actor.rs`): the operator's staff are the
 * operator, anybody else is named, a rule or machine is described in words,
 * and an actor that cannot be found is not guessed at. This only says it.
 */
export type Actor =
  | { kind: 'person'; name: string }
  | { kind: 'operator' }
  | { kind: 'system'; name: string }
  | { kind: 'unrecorded' }

/** An actor in words, or nothing where none was recorded or the API is older. */
export function actorName(actor: Actor | undefined): string | null {
  switch (actor?.kind) {
    case 'person':
    case 'system':
      return actor.name
    case 'operator':
      return 'the operator'
    default:
      return null
  }
}

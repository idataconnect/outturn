# Prompt caching

Which parts of a prompt a provider is asked to cache, and how compaction keeps
the cached parts worth having.

Designed, unbuilt. What exists is measurement: the usage ledger records
`cache_read_tokens` and `cache_write_tokens` for every model call, from what
each provider reports. Nothing asks any provider to cache anything.

## What each provider does

Checked against memory rather than current documentation, so verify before
building on the specifics.

- **Gemini** caches implicitly on recent models: a request sharing a prefix with
  a recent one may be billed partly at the cached rate. Best effort, no charge
  for the write, nothing to ask for. Explicit caching exists -- a cached content
  object, billed for storage by the hour -- and is not used.
- **OpenAI** caches automatically past about 1,024 prompt tokens, with no write
  charge.
- **Anthropic** caches only what the request marks with `cache_control`, up to
  four breakpoints per request, and charges more than ordinary input for the
  write. A cached prefix lasts about five minutes, renewed by each hit, and is
  read by any later request that starts with exactly the same text.

So for Anthropic every turn today is uncached input, and the settings below
are what change that. Gemini and OpenAI ignore them.

## Breakpoints

Placed where the prompt stops changing, one per kind. Each is a setting in the
cascade ([settings.md](settings.md)), so an operator, workspace or agent can
turn any of them off.

| setting | default | where it goes |
|---|---|---|
| `cache_instructions` | on | after the preamble, the system prompt and the skills |
| `cache_compacted_history` | on | at the last summary or compaction boundary |
| `cache_conversation` | on | at the end of the last completed turn |
| `cache_tool_rounds` | off | at the end of each round of the current turn's tool loop |

**Instructions** change only when somebody edits the agent or a skill, so they
are shared by every session of an agent. A busy agent keeps this prefix warm for
all its users, which makes it the breakpoint most likely to survive a slow
reply.

**Compacted history** holds from one compaction to the next, so a change later
in the conversation misses only from that point on.

**Conversation** stops at completed turns. The current turn's tool calls and
results are sent at full price on each round; once the turn ends they are part
of the history, and the next turn's first request caches them once, in
whatever form the projection gives them. A turn is cached when it has settled
rather than while it is still growing.

**Tool rounds** moves forward inside the loop. Off by default, because a tool
result is the largest and shortest-lived thing in a prompt, and the write
premium buys little on something compaction will stub first. Worth turning on
for an agent whose turns run many rounds over a large, stable working set.

Four kinds, four breakpoints: Anthropic's limit is met exactly, and never
exceeded, whichever are on.

## Compaction has to cooperate

The cache rewards a prefix that stays the same, and compaction exists to change
it. Trimmed a little every turn -- the oldest tool result stubbed, then the next
-- the history's start moves on every request once a session is over budget,
and everything after the change misses every time.

So compaction runs with hysteresis. When a session crosses its budget it is cut
to a mark well below it, in one step, and then left alone until it crosses
again. Each compaction costs one miss rather than one per turn, and the
compacted-history breakpoint stays good between them. The order of sacrifice
in AGENTS.md is unchanged; it is the batching that is new.

Summarising completed turns' tool results fits the same rule. Done once, when
the turn has finished, it is a single miss, after which every turn caches and
sends the smaller version.

## Measuring it

The ledger already separates cache reads and writes, so whether a breakpoint
pays is a query rather than an estimate: write tokens against the read tokens
that followed, per agent. A breakpoint whose writes are rarely read is one to
turn off, which is what the settings are for.

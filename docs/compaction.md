# Compaction

Built: `api::chat::summarize`, `worker::summarized` and `worker::store_summary`,
under the `context_budget` setting, with the trim below it as the floor. The
design, so it is not rediscovered:

A transcript outlives any model's window, and the window is a property of the
route rather than of the session — so a turn can arrive at a smaller context
than the one before it. The naive ordering, compact in the outgoing model
before switching, is a trap: it bills the user for an expensive operation they
did not ask for, at the moment they asked for something else.

Summarize from the system prompt, the previous summary, and the tail. That
input is bounded by construction, so the incoming model can always do it
however long the session has run, and no compaction depends on a model that is
being switched away from. The tail is also where the live context is: what is
being worked on now, the recent tool results, the thread of the conversation.

Summaries are cumulative — each one summarizes the tail plus the summary before
it — because the alternative loses durable facts. Constraints stated once at the
start are exactly what gets dropped and then violated. Cumulative carrying is
not a guarantee, only a much better chance.

The better fix is **compaction carry-over**: marking something as needing to
survive, so compaction does not have to guess what was load-bearing. Named for
the mechanism rather than the promise, because it is bounded — a carry-over is
what a summary takes with it when there is room, not a guarantee of
permanence, and a name like "kept" would promise what the bound cannot deliver.
What must not be quiet is the bound: something dropped at it should say so,
or a summary becomes the record of a fact nobody can see leaving.

**Carry-over is not memory, and the two must not share a store.** Memory is
user-declared and durable — "I am off on Thursdays, so never set a pay date
there, whatever the skill says" — stated once, applying to every session, and
expected to hold. Carry-over is one model's judgment about one conversation,
and it belongs to that session's transcript. Mixing them makes memory
unreviewable: a standing instruction somebody gave and a guess a model made
about a transcript become indistinguishable a month later, and nobody can say
why the agent believes something. They solve different problems and are
independently buildable; neither is a prerequisite for the other.

A summary is a message a model wrote about the conversation and will be
replayed on every later turn, so its failure mode is quiet: a summary that
misstates a decision becomes the record. Mark it as a summary in the
transcript rather than folding it in as ordinary history — both so a reader can
see what happened, and so the next compaction knows it is compacting a summary.

The mark is `metadata.summary_through`, naming the last message the summary
stands in for. It is what both halves of that sentence rest on, and each of them
has already been got wrong once. Withholding summaries from the reader's page
was tried and reverted: it left a person unable to see that their conversation
had been compacted at all, which is the quiet bound this section warns against
two paragraphs above. Serving them unmarked is the other failure — a paragraph
summarizing the reader's own conversation, presented as something the agent said
to them. They are served, and the client draws them as the boundary they are.

On the way *to* the model a summary is labeled too (`summarize::framed`), and
every summary is, not only the newest: an older one can survive inside the
retained tail, and unlabeled there the agent reads its own summary as something
it said and answers it. What a summary covers is recorded from the projection's
own account of which stored message each entry came from, never by counting the
projection against the stored rows — they are not aligned, a stored summary
breaks the alignment by exactly one, and counting silently dropped a message per
round from every long session.

Underneath all of it, a trim that cannot fail. No model call, so it works when
the provider is down, the breaker is open, or the summary itself would not fit.
It is what guarantees a user never sees "context exceeded", which is the actual
requirement — everything above is about doing better than that.

**Drop by what a message is, not by how old it is.** An earlier draft said to
drop whole turns from the oldest end, which assumes age tracks irrelevance. In
the sessions this platform is for, that is close to backwards: a workspace
employee onboarding a customer runs for hours and calls tools constantly, and
the oldest turns are where the premise was set — which customer, which system,
what the constraints were — while the middle fills with tool results that were
consumed the moment they arrived. Oldest-first discards the brief and keeps the
mechanics.

So the order of sacrifice is by kind:

1. **Tool results, oldest first.** They are the bulk in a tool-heavy session,
   they are usually spent on arrival, and losing one is recoverable — the agent
   can call the tool again. Replaced by a stub rather than removed, because a
   call with no answer is a request both protocols reject.
2. **Whole assistant/tool round trips**, oldest first, once stubbing is not
   enough.
3. **Ordinary conversation turns**, oldest first, last.

The first user message is what a session is *for*, and is the first thing an
oldest-first rule throws away. Whether that earns an explicit exemption or
whether it falls out of ordering tool results ahead of conversation is worth
settling with a real transcript rather than by argument.

**A stub must say it is a stub.** A dropped result replaced by something that
reads as real output is a silent lie, and the model will reason from it. Say
the tool ran and its output was dropped to fit, so the agent can call again if
it mattered. This is the same principle as marking a summary as a summary.

**Always keep the last user message**, since a turn with nothing to answer is
already an error in the guest.

Budgets belong on the route, beside `model`, because the window is a property
of the model. But the trim runs where the conversation is built, in the API,
and the API has no routing; today no routes are seeded at all, so most turns
would have no budget to read. Settle this before building: a setting that
cascades like every other, with a route override when the gateway grows one, is
the shape that works from the first turn. Compact against a fraction of the
window rather than the whole of it, leaving room for the reply, for tool
results arriving mid-turn, and for the compaction call itself.

The system prompt is **spent**, not compacted. It goes to the model on every
round exactly as the conversation does, but it is a separate field nothing can
trim -- composed once per conversation and again at compaction, see
[docs/prompt-contributors.md](prompt-contributors.md) -- so it comes out of
`context_budget` before the conversation gets any (`trim::room_for_conversation`).
Uncounted, the budget quietly meant something else: ten long skills could
exceed it on their own while the trim reported the conversation comfortably
inside, and the turn failed at the provider's real limit with no trim log and
nothing to explain it. Anything that later contributes to the prompt -- carry-
over is the obvious candidate -- is spending the same way and must be counted
the same way.

Compact with hysteresis: cut to a mark well below the budget in one step, then
leave the prefix alone. Trimming a little every turn changes the start of the
history on every request, and a provider's prompt cache misses from there on
each time -- see [docs/caching.md](caching.md).

Nothing counts tokens anywhere in this codebase. A per-model tokeniser is a
dependency that is wrong for every model it was not built for; bytes over a
conservative budget is approximate in the safe direction, and being wrong costs
headroom rather than a failed turn.

The gateway must eventually support mid-session provider failover — an
Anthropic outage substituting Gemini and continuing. That requires separating
the durable transcript from the projection sent to a model, so provider-specific
artifacts (Gemini thought signatures, Anthropic thinking blocks, differing tool
call shapes) are annotations filtered per target rather than facts about
storage. Lossy parts should degrade, never fail the turn.

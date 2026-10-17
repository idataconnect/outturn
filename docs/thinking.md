# Thinking

How a model's reasoning is asked for, shown, kept and given back. Designed;
nothing here is built. Anthropic is the provider it is mostly about, because it
is the one whose reasoning has to make a round trip: the others either stream
it as text to show or keep it to themselves.

## What goes wrong today

An Anthropic route asks for nothing about thinking, and gets it anyway. The
current Claude models think adaptively when a request does not mention
thinking, so every turn on Sonnet 5.5 has been reasoning, at the provider's
default effort, inside a `max_tokens` of 4096 that the translation fills in
because the field is required. Three things follow:

- **The reasoning is invisible.** It streams as `thinking` blocks, which
  `translate.rs` notes and drops. On these models their text would be empty
  in any case: what is returned is a summary, and only when asked for.
- **A hard turn can be cut off before it answers.** `max_tokens` caps
  everything the model writes, thinking included, and the model is not told
  where the cap is. It stops mid-thought with `stop_reason: "max_tokens"`.
- **Its reasoning did not survive the tool loop.** Each round replayed the
  conversation without the thinking blocks of the rounds before it, so the
  model started every round without the reasons for the call it just made.
  Now kept within a turn; see step 4 below.

## What the provider does

Facts the design rests on, for the Claude 5.x models. Per model, since they
differ -- Haiku 5.5 most of all.

- **Thinking is adaptive, and on Sonnet 5.5 and Opus 5.5 always on.** Sending
  `{"type": "disabled"}` to either is a 400. The control is effort (`low`,
  `medium`, `high`, `xhigh`, `max`), and it is advisory: less thinking, fewer
  tool calls and shorter answers, but no ceiling. There is no thinking budget
  any more; `budget_tokens` is a 400. Haiku 5.5 can have thinking turned off.
- **`display` decides what comes back, not what is done.** `omitted` -- the
  default -- returns thinking blocks with empty text; `summarized` returns a
  digest written alongside the reasoning, never the reasoning itself. The
  thinking is billed the same either way. A beta `updates` returns only the
  short notes the model writes between tool calls, which on these models
  arrive as thinking blocks rather than as text.
- **A thinking block carries a signature, and has to go back as it came.**
  The signature streams after the text, in a chunk of its own with no text.
  A block is valid only for the model and the conversation that produced it;
  another model ignores it, and editing the history before it invalidates it.
  A block with empty text and a signature is still a block, and still goes
  back.
- **Sampling parameters are refused.** `temperature`, `top_p` and `top_k` are
  400s on the 4.7 and later and 5.x lines, except a `temperature` of exactly 1.
- **Forced tool choice is refused** on Sonnet 5.5, Opus 5.5 and the 5.1 line:
  `tool_choice` of `any` or a named tool is a 400. Haiku 5.5 accepts it.
- **System messages may sit mid-conversation** on the 4.8 and later models: a
  `role: "system"` entry in `messages`, after a user turn, never next to
  another. One with empty content and an `output_config` changes effort from
  that point on (beta), which is how effort changes without changing the front
  of the request.

## Asking

The Anthropic translation sends, on a model that thinks:

- **`thinking: {"type": "adaptive", "display": "summarized"}`.** Summarized,
  because the person reading the transcript should see why a turn did what it
  did, and the cost is the same.
- **Effort from `reasoning_effort`**, as `output_config.effort`. `minimal`
  becomes `low`. `none` leaves `thinking` out and asks for the lowest effort
  the model offers -- never `disabled`, which the always-on models refuse, and
  leaving `thinking` out does not stop them thinking. A level the model does
  not offer is clamped to the highest it does.
- **`max_tokens` at the model's output limit**, not 4096. A large cap costs
  nothing unless it is used, and a small one turns a hard question into a
  truncated answer. Effort is the control; the cap is only the guarantee.
- **No sampling parameters** to a model that refuses them, and no forced tool
  choice to one that refuses that; `required` becomes `auto`.

Which model is which comes from the [model catalog](models.md): always-on
thinking, the effort levels offered, whether sampling and forced tool choice
are accepted, whether mid-conversation system messages are, and the output
limit. The catalog's source does not carry all of these, so the gaps are
filled from defaults keyed on the model family, with an override per route as
for any other field. A Claude id nothing recognizes is treated as the newest
generation -- adaptive, no sampling -- since a new model is far likelier to
resemble the last one than the first.

## Showing

`thinking_delta` becomes `reasoning` on the OpenAI-shaped chunk, where the
runtime already looks for qwen's, so the reasoning panel needs nothing new.
`redacted_thinking` shows as a placeholder rather than as nothing, since a
turn that thought and says so is more honest than one that seems not to have.

## Keeping

**The signature has to survive the whole stack, which is the real work
here.** Reasoning travels today as text: `reasoning` on a chunk, a
`Part::Reasoning { text }` in the transcript. Text cannot be given back; a
signed block can. So a reasoning part gains what giving it back needs -- the
signature, or a redacted block's opaque data -- and the model and route that
produced it, from the gateway through the runtime and the guest to what is
stored.

**Given back as a thinking block, never as speech.** `Part::Reasoning` is
never sent to a model as a past utterance, because a model handed its own
reasoning as something said reads it as speech and answers it. That holds. A
signed block is not speech: it goes back in the shape it came, as a thinking
block, first in its assistant message, and only to the model that wrote it.
To any other model, including after a route's fallback, it is left out
altogether -- not turned into text. A block with no signature, from a stream
that was cut off, is left out too.

**Within a turn, every round.** The tool loop is where losing it costs most,
and where the provider most expects it: a reply that called a tool goes back
with the thinking that preceded the call. Across turns as well, since these
models are built to read their earlier reasoning -- subject to the next
section, because a history that changes underneath it makes it invalid.

## The front of the request does not change

The same rule [caching](caching.md) runs on, for a second reason. A thinking
block is bound to the conversation before it; edit that, and every block after
the edit is invalid. So:

- **Effort changes go in as a message, not a parameter.** The request-level
  effort stays fixed for a session, and an agent's change of effort is a
  mid-conversation system message with an `output_config` and no content,
  placed where it applies. Changing the top-level parameter would invalidate
  the cached history as well.
- **Stale blocks are dropped rather than refused.** The beta
  `block_binding: {"prefix_mismatch_behavior": "drop_block"}` makes the
  provider discard a block whose prefix changed, instead of answering 400 for
  the rest of the session. Compaction rewrites history by design, so this is
  what lets it and thinking coexist.
- **Mid-conversation system messages are placed by one rule.** After a user
  turn, never beside another, runs merged; one that would end the request, or
  sit right before an assistant turn, becomes a user turn instead. Placement
  depends only on the messages before it, so each request is a byte-identical
  prefix of the next. That also fixes what the translation does now with more
  than one system message, which is keep the last and lose the rest.

## Order of work

1. **Ask properly.** Built, apart from forced tool choice, which nothing sends
   yet: `display`, effort from `reasoning_effort`, `max_tokens` at the
   model's limit, and sampling dropped where refused, from the model's name
   until the catalog exists (`ClaudeTraits` in `translate.rs`).
2. **Show it.** Built: `thinking_delta` streams as `reasoning`. Redacted
   thinking is still dropped rather than shown as a placeholder.
3. **Stream one real tool-using turn.** Done: a summarized block streams as
   many small `thinking_delta` chunks, a few words each, followed by one
   `signature_delta`, and the notes between tool calls arrive as small
   thinking blocks shown between the calls. An easy question at medium
   effort may not think at all.
4. **Keep it.** Built. The gateway assembles each signed block and sends it
   once it closes, as `thinking_block` on a chunk beside the `reasoning` it
   already streamed; an unsigned one, from a cut stream, is let go. The
   runtime holds a reply's blocks and any signatures on its calls, by the
   reply's first call id, with the model the gateway said served it, and puts
   them back when the guest replays that reply -- as `thinking_blocks` and
   `signed_by` on the message and `provider_signature` on each call. The
   guest never sees them, so the agent interface did not change. A finished
   turn reports what it signed, the API keeps it in `reply_signatures` --
   beside the reply rather than in it, so a transcript read never carries
   what only the model can use -- and the next turn is handed back those
   whose calls are still in its conversation.

   Which model may read them is the gateway's call, since only it knows where
   a round is going. Anthropic's API drops a block the receiving model cannot
   read, unbilled, and asks for blocks to be passed back regardless, so the
   gateway sends them. Gemini refuses a call another model signed, so the
   gateway renders that exchange as prose instead. And because a block given
   back after anything before it changed is a 400 on newer accounts -- a
   system prompt composed afresh, history trimmed or compacted -- every
   adaptive request asks for `block_binding.prefix_mismatch_behavior:
   "drop_block"` under the `thinking-binding-controls-2026-08-01` beta: a
   stale block costs its own reasoning and nothing more.

   **A refusal it can cause.** With its earlier reasoning in the
   conversation, a follow-up such as "Why did you do it that way?" is
   declined as `reasoning_extraction` -- every time, in an A/B against the
   real API on 2026-10-16, while the same request without the thinking was
   answered every time, and "Why did you check the clock just now?" was
   answered either way. Anthropic's own fallback does not retry that
   category. So the gateway does, once: a request that carried thinking and
   was declined as reasoning extraction before saying anything is asked
   again without the thinking. That is logged, and marked on the usage the
   ledger keeps (`outturn.retried_without_thinking`, with the declined
   attempt's usage beside it), so how often it happens is a query rather than
   a guess. Every other refusal is passed on as one: the turn ends saying
   why, and is not retried.
5. **Place system messages by the rule**, and effort as a message.
   `drop_block` is on already, with step 4.

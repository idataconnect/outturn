# Session search

Finding a conversation again. Designed, not built.

## Why the list stops being enough

The sessions sidebar loads every session the reader can see, newest first by
creation, with no limit. That was an audit finding before it was a UI
complaint: the query stops only when it runs out of rows, and the list it feeds
grows for as long as the workspace exists.

Paging it fixes the query and not the problem. Nobody scrolls through four
hundred conversations looking for the one about the Rose Room; they remember a
word from it, or roughly when it was, or which agent it was with. So the list
becomes *recent*, and everything older is reached by search. The two are
designed together because they share a query, an order and a cursor.

## What the reader is served

**Recent is by last activity, not by creation.** A conversation somebody
picked up again this morning belongs at the top, however old it is.
`agent_sessions` gains `last_active_at`, written when a message is stored --
the same statement, so it cannot drift -- and the list orders on
`(last_active_at desc, id desc)`. The id breaks ties, because timestamps
collide and a keyset cursor over a column that collides skips rows.

That replaces `agent_sessions_workspace_idx (workspace_id, created_at desc)`
with `(workspace_id, last_active_at desc, id desc)`. Nothing else reads the old
order.

**One endpoint, a page at a time.** `GET /v1/agent-sessions` takes `q`,
`agent`, `before` and `limit`, and answers with an envelope rather than a bare
array:

```json
{ "sessions": [...], "next": "<cursor or null>" }
```

The cursor is opaque to the client -- it is `(last_active_at, id)` for the
recent list and something else for a ranked search, and the client should not
have to know which. `limit` is clamped at the handler, the way `/v1/usage`
already does it. The envelope is the one contract change, and it is why this
endpoint changes once rather than twice.

**A hit says why it matched.** A search result carries the session and, when it
matched on content rather than title, the message it matched and a short
excerpt around the match. A title the namer wrote after the first turn is often
not the word somebody remembers.

## Narrowing belongs in the query

Searching message contents is reading transcripts, so it is narrowed exactly as
reading them is: the agents the caller was scoped to, plus their own
conversations whichever agent those are with. `Visible::of` already states that
rule for the event feed, and search takes it as a parameter the same way.

It is applied in SQL, not to the results. Filtered afterwards, a page of ten
hits that are all invisible comes back empty and reads as "nothing matched",
and the count of what was hidden is itself a leak: a narrowed person who can
see that a search for a colleague's name matched something has learned it.

## Lexical search, built in

Postgres full-text search. Nothing new to deploy: `pg_trgm` ships with
Postgres and needs only a `create extension` in the migration.

- **Titles** by trigram (`pg_trgm`), because they are short, typed by people,
  and a partial word should find them. `ilike` over a GIN trigram index.
- **Content** by `tsvector`, as a generated column on `agent_messages` with a
  GIN index, queried with `websearch_to_tsquery` so quoting and `-word` do what
  a person expects.

The text search configuration is `simple`, not `english`. Stemming is the
right call for one language and the wrong one for the rest, and a workspace
running a French front desk should not find its search quietly worse than an
English one. Revisit when somebody asks for stemming, per workspace, as a
setting that cascades like every other.

**What is indexed.** User and assistant messages. Not:

- *Tool results.* They are the bulk of a tool-heavy session, mostly a response
  body nobody would search for, and they are where fetched content lives -- a
  page the agent read is not something the reader said or was told.
- *Summaries* (`metadata.summary_through`). A model wrote them about the
  conversation; a hit on one points at words that are not in the transcript the
  reader will open.
- *Replies still streaming.* A reply is created empty and filled; the generated
  column follows the row, so this falls out rather than being a rule.

## Semantic search, as an optional component

"The conversation where we sorted out the double booking" does not share a word
with the conversation it means. Embeddings find it. They are also a model,
a GPU's worth of memory and a backfill, which most installations should not
have to carry to get a search box. So, like Tika: a component that arrives with
its configuration, and whose absence is a search that is lexical and says
nothing about it.

**The pieces.**

- `k8s/components/embeddings` serves an embedding model -- Snowflake's
  `arctic-embed-l-v2.0` is the suggestion: multilingual, 1024 dimensions, and
  truncatable to 256 with little loss when storage matters more. Served over
  the OpenAI embeddings protocol, which llama.cpp, TEI and ollama all speak.
- `OUTTURN_EMBEDDINGS_MODEL` on the API says semantic search is on and which
  model answers it. Unset, nothing is embedded and nothing is attempted.
- `message_embeddings (message_id, model, embedding vector(1024))`, with an HNSW
  index. pgvector is already in the Postgres image and the extension is already
  created, so this is a migration and nothing else.

**Through the gateway, not beside it.** The obvious wiring -- the API calls the
embeddings pod directly, as it calls Tika -- is wrong here, and the difference
is the point. Tika is a parser. An embedding is a model call, and every model
call is a row in the usage ledger, made by the tier that holds credentials. An
in-cluster model needs no key today, but the same setting pointed at a hosted
embeddings API needs one, and the code path that would have been skipping the
ledger would then also be holding a secret in the wrong tier. So embeddings are
a gateway operation, routed and attributed like chat: the workspace that owns
the message pays for embedding it, and the ledger says so.

**When things are embedded.** After the fact, by the worker, as a job per
finished message -- never inside the request that stored it, and never on the
turn's hot path. A backfill job walks what has no embedding for the current
model, oldest-first per workspace, at low priority, so enabling the component
on a busy install is a queue that drains rather than a migration that locks.

**Changing model.** The row carries its model, and a query only compares
vectors from the model that made the query's own. Switching model starts a
backfill; until it finishes, search is hybrid over whatever has been
re-embedded and lexical over the rest -- degraded, never wrong. A different
dimension means a different column and index, which is a migration somebody
chose, not something a setting can do.

**Ranking both.** Reciprocal rank fusion over the lexical and the vector result
lists. It needs no tuning and no score calibration between two measures that
were never on the same scale, which is the whole reason to prefer it to a
weighted sum.

## Order of work

1. `last_active_at`, the new index, and the paged envelope on the recent list.
   The UI's sidebar reads pages and says when there are more. This alone
   retires the audit finding.
2. Title search. Most of what people type, for a trigram index.
3. Content search, narrowed in the query, with the excerpt.
4. The embeddings component, the gateway operation, the worker job and the
   backfill.

Each is useful without the next. Nothing in 1-3 needs a decision this document
has not made; 4 needs one about how the gateway routes an operation that is
not chat, which [routing.md](routing.md) should settle rather than this.

## Not here

- **Searching files.** Tika already turns uploads into text, and the same
  index could take it -- but a file's visibility is its storage scope rather
  than a session's, and that rule deserves its own paragraph somewhere it can
  be argued with.
- **Search across workspaces.** One read across every workspace a person
  belongs to is what the action queue does, and search may want it too. It is
  a different narrowing question, and the answer is not "whatever the token
  says".
- **Agents searching their own history.** That is a tool, with a tool's
  question of what a turn may read, and it is closer to memory than to this.

# Skill operations as tools

A skill's operations, offered to the model as tools, and translated by the
guest into the `fetch_url` request the operation's own file already describes.
Designed, not built.

## The problem

Calling a skill's operation today takes three mechanisms, and a model has to
know which is which:

| Wanting | Means |
|---|---|
| a tool | `load_tools`, then the tool |
| what an operation is | `read_object("skill/<name>/<operation>.md")` |
| to perform it | a `fetch_url` request composed from that file: method, URL, headers, body |

Every gap between them has been fallen into. Small models, one afternoon, one
guesthouse:

- asked `load_tools` for `check_availability` and `create_booking`, which are
  operations and not tools, and were handed back the list of things they could
  have asked for;
- fetched `skill/hollowbrook/list_rooms.md` from a host invented for it, rather
  than reading it;
- posted a booking without `Content-Type`, was refused `415`, and resent it
  twice -- one of which took the room.

None of that is a model being unreasonable. "Can I do this?" has three answers
depending on what *this* is, and the model is expected to read a document to
learn the shape of a request the platform could have built for it. A larger
model copes, but coping is not the same as being given the right interface, and
an evicted pod's retry does not read documentation either.

## The shape

An operation is a tool. Its file says what it is, and the guest does the rest:

1. **The file declares the operation** in its frontmatter, beside `approval`
   where one exists:

   ```yaml
   ---
   operation:
     method: POST
     url: http://outturn-hollowbrook:8084/bookings
     body: [room_id, guest_name, arrival, departure]
   ---
   ```

   Arguments are flat lists of names, not a schema, because the frontmatter
   parser reads scalars and flat lists and refuses everything else (see
   `crates/api/src/skill/frontmatter.rs` for why it is that narrow):

   - `body` and `query` name the JSON body's fields and the query string's
     parameters. A `GET` or `HEAD` has no body.
   - A path parameter is a whole segment of the URL, `/bookings/{id}`, and is
     always a required string.
   - `optional` names the body or query fields that may be left out; the rest
     are required.
   - `integer`, `number` and `boolean` type the fields that are not strings.
   - `list` names the fields that are lists of their type: a query parameter
     sent repeated, `tags=a&tags=b`, or a JSON array in a body. A list of
     integers is in both `list` and `integer`.

   A name lives in one place, and a qualifier must name a field that is sent:
   a typo in `optional` would otherwise leave required the field it meant.

2. **The guest offers it as a deferred tool**, `hollowbrook_create_booking`,
   named by skill and file so two skills cannot collide, and reduced to the
   letters, digits, `_` and `-` every provider accepts in a tool name. Its description
   is the first paragraph of the file's prose -- what the operation *is* -- and
   its parameters are the declared arguments. `load_tools` loads it like any
   other -- and by the name its skill's manifest lists it under,
   `create_booking`, where one bound operation has that name. That is the
   name a model reading the manifest asks for, so the call every small model
   tried first is now the right one.

3. **A call becomes the `fetch_url` request** the file describes: the method,
   the URL with its path parameters filled in and encoded, the query,
   `Content-Type: application/json` and the body. A call missing a required
   argument, or carrying one the operation does not take, is not sent: half a
   request that reached the recipient may already have done something. It goes out through the same host import, so
   the result the model sees is the one `fetch_url` would have returned.

The prose stays, and stops being required reading. The body of the file is
still where judgment lives -- charge only after a booking, quote the id to the
guest -- and `read_object` still reaches it. What the model no longer has to
get from it is the shape of a request.

`fetch_url` stays too, for everything that is not a declared operation. A
skill without `operation` blocks works exactly as it does today.

## Why a translation, and where it happens

[roadmap.md](roadmap.md), *Tool registration*, left this as a fork: a
registered tool in the guest needs its definition to cross the WIT boundary,
and in the host it is a tool the component does not reason about. Both are
right about *registering* a tool. This is narrower. It registers nothing: an
operation is a declaration the API has already read, translated into a call
the guest can already make.

**The API parses; the guest is handed the result.** The frontmatter is parsed
at publish, where a malformed declaration is refused to the person publishing
it. The guest cannot share that parser -- it is a wasm component whose one
dependency is a JSON library -- and reading and parsing every operation file
at the start of every turn would be work done again for an answer that cannot
have changed. So the turn carries the bound skills' operations, already
parsed, and the guest asks for them through one host function,
`skill-operations`, as it asks for `eager-tools` now. That is the only
interface change, and it adds a question rather than changing an answer.

Everything downstream is untouched:

- **No gateway change.** The request that arrives is an ordinary egress
  request. The egress rule, the credential binding, the approval gate and the
  commitment all apply to it as they would to the same request composed by
  hand -- because it *is* the same request.
- **Nothing to trust.** A guest that translates can only produce a request
  it could already have sent through `fetch_url`. A compromised runtime that
  mistranslates gains nothing it did not have, which is the property every
  other part of the agent interface is held to.

## Why it is not brittle

The declaration is the one place the operation's shape is written. The tool
the model is offered and the request the guest builds are both read from it,
parsed once by the API, from the version the turn bound. They cannot drift, because
there is nothing to drift between.

What can disagree is a gate and an operation written in one file by hand:
`approval.matches: POST /charges` beside an `operation` whose URL is
`/charge`. The parser refuses that at publish -- a gate in a file with an
operation must match that operation -- so the disagreement is found by whoever
wrote it rather than by an agent that could not be charged for.

## Outside outturn

A skill here resembles the public Agent Skills convention without claiming
compatibility ([skill-packages.md](skill-packages.md)), and an operation file
is already a level that convention has no equivalent of. `operation` changes
nothing there. What it would change is the prose: once the tool builds the
request, the "The call" section that told a model the method and URL is
redundant here -- and the only thing an agent elsewhere had to go on.

So the prose may drop it, and wherever a skill leaves this platform the line
is generated back from the declaration, so it is still written once. Nothing
exports a skill yet; this is what an export has to do when one is built.

## The wizard

The OpenAPI wizard already writes these files from a specification; it writes
the `operation` block from the same operation it writes the prose from, so a
wizard-built skill gets tools without anyone typing a schema.

## What it leaves to the host

Idempotency, by the argument in [idempotency.md](idempotency.md), *Why the
host owns it*. An `operation` block is also where its identity is declared --
`identity: [room_id, guest_name, arrival, departure]` -- and that has to reach
the gateway the way a gate's `binds` does, through the commitment, so the key
is derived on the host from the request it actually sees. The translation
needs none of that to be useful, and lands first.

## Hollowbrook

Its operation files gain `operation` blocks, and their prose loses the "The
call" sections that described the request by hand. It is the test of whether
the shape is right: a booking that small models spent an afternoon fumbling should be
`load_tools` and one call.

## Not built

In order:

- ~~`operation` in the frontmatter parser, with the publish-time check
  against a gate in the same file.~~ Built.
- ~~The operations stored with a version, and carried to the turn.~~ Built.
- ~~`skill-operations` in `wit/agent.wit`, answered by the runtime.~~ Built.
- ~~The guest offering declared operations as deferred tools, and translating
  a call into `fetch`.~~ Built.
- ~~The wizard writing `operation` blocks.~~ Built; see
  [openapi-wizard.md](openapi-wizard.md), *Operations as tools*.
- Hollowbrook's files converted, and an end-to-end test against the fake
  gateway that books a room through the tool.
- `identity`, with [idempotency.md](idempotency.md).

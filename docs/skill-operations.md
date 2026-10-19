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
     body:
       type: object
       required: [room_id, guest_name, arrival, departure]
       properties:
         room_id: { type: string }
         guest_name: { type: string }
         arrival: { type: string, format: date }
         departure: { type: string, format: date }
   ---
   ```

   Path parameters are named in the URL, `/bookings/{id}`, and appear in the
   schema like any other argument; query parameters are declared under
   `query`. A `GET` has no `body`.

2. **The guest offers it as a deferred tool**, `hollowbrook.create_booking`,
   named by skill and operation so two skills cannot collide. Its description
   is the first paragraph of the file's prose -- what the operation *is* -- and
   its parameters are the declared schema. `load_tools` loads it like any
   other, so the call every small model tried first is now the right one.

3. **A call becomes the `fetch_url` request** the file describes: the method,
   the URL with its path parameters filled in, the query, `Content-Type:
   application/json` and the body. It goes out through the same host import, so
   the result the model sees is the one `fetch_url` would have returned.

The prose stays, and stops being required reading. The body of the file is
still where judgment lives -- charge only after a booking, quote the id to the
guest -- and `read_object` still reaches it. What the model no longer has to
get from it is the shape of a request.

`fetch_url` stays too, for everything that is not a declared operation. A
skill without `operation` blocks works exactly as it does today.

## Why a translation, and why in the guest

[roadmap.md](roadmap.md), *Tool registration*, left this as a fork: a
registered tool in the guest needs its definition to cross the WIT boundary,
and in the host it is a tool the component does not reason about. Both are
right about *registering* a tool. This is narrower. It registers nothing: an
operation is a file the guest can already read, translated into a call it can
already make.

So it touches less than the fork suggests:

- **No interface change.** The guest reads the bound skills' files through
  `read_object` and their operations through the same parser the API runs at
  publish, and it calls `fetch` as it does now. `wit/agent.wit` is unchanged.
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
by the same parser, from the version the turn bound. They cannot drift, because
there is nothing to drift between.

What can disagree is a gate and an operation written in one file by hand:
`approval.matches: POST /charges` beside an `operation` whose URL is
`/charge`. The parser refuses that at publish -- a gate in a file with an
operation must match that operation -- so the disagreement is found by whoever
wrote it rather than by an agent that could not be charged for.

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

- `operation` in the frontmatter parser, with the publish-time check against a
  gate in the same file.
- The guest offering declared operations as deferred tools, and translating a
  call into `fetch`.
- The wizard writing `operation` blocks.
- Hollowbrook's files converted, and an end-to-end test against the fake
  gateway that books a room through the tool.
- `identity`, with [idempotency.md](idempotency.md).

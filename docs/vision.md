# Looking at an image

An agent can be handed an image and ask a model what is in it. This is how,
and why it is a tool rather than a kind of message.

## The obvious design, and why it is wrong

A model that can see takes images in its messages, so the obvious move is a
content part: add `image` beside `text` and `call` in the WIT, attach it to a
message, let the model look.

Two things spoil it.

**Every route in a chain must be able to see.** A fallback that drops to a
text-only model does not degrade, it breaks: the conversation contains
something the model cannot read. `traffic_routes` exists to make failover
routine, and this would make it dangerous.

**A pass over an image is directed.** Asking "what is in this screenshot" and
asking "what is the phone number in the corner" attend to different things and
produce different answers. A description written when the image arrived was
written before anyone knew the question -- so it is not merely lossy, it is
lossy in a direction chosen in advance. The second question is asked in the
middle of a conversation, by the agent, and nothing at upload time could have
anticipated it.

Together those say: keep the bytes, and let the asking happen more than once,
with a question each time.

## What is built

**The image is stored, and the transcript holds a path.** Same objects, same
scopes, same refusals as every other file. Pasting one into the composer
uploads it to the session scope and mentions where it went, so the bytes
outlive the turn and can be asked about again.

**`describe-image(path, question)` is a host function.** The guest names an
object and says what it wants to know. The host resolves the path, reads the
bytes, and asks a model that can see; what comes back is text, which the agent
reasons about like any other tool result.

The guest never receives the bytes. That is the same boundary `read-object`
already keeps for documents -- a PDF comes back as its words -- and it is what
stops an image being something a compromised component can carry off through
the one channel it has.

**It is a tool, not a content part, because it carries intent.** A content part
says "here is an image"; a call says "here is an image, and here is what I want
to know about it". The second is what a directed pass needs, and it is what
makes asking twice sensible rather than redundant.

**The model is a routing decision.** The call goes out under the `vision`
traffic type, so which model serves it is a row in `traffic_routes` rather than
anything in code -- an operator on qwen3.8 locally, a workspace pointing at
something else, the same cascade as everything. A deployment with no vision
route configured gets a refusal that says so, rather than a model being asked
to look at something it cannot see.

## What this costs

A call per question. There is no description cached at upload and none written
on a turn nobody asked -- an image nobody asks about costs one upload and
nothing else. An image asked about three ways costs three calls, which is the
price of three answers that are actually about the three questions.

If the same question is asked twice the second call is wasted. Worth caching
eventually, keyed on the object and the question; not worth it before anyone
has seen the numbers.

## What is not built

- **No image content part in the WIT.** A vision-capable route could be handed
  the image itself, which would let a model attend to it across a whole
  conversation rather than through one answer at a time. It is the better end
  state for models that can see, and it is additive: this design does not
  foreclose it, and the stored path is what it would use.
- **No automatic description on upload.** An image sitting in a session is
  bytes until something asks about it, for the reason above: a description
  written before the question is a worse answer than one written after.
- **Nothing reads text out of an image on its own.** Document extraction turns
  a PDF into words because the words are the document. An image's description
  is an interpretation, so it happens when somebody asks for it.

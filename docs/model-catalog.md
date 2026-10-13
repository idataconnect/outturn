# Model catalog

What a model accepts, written down once, so a route does not have to learn it
from a 400. Designed; nothing here is built.

## Why

Moving local development onto Claude Sonnet 5.5 failed every turn with

    guest failed: guest returned an error: gateway returned 502 Bad Gateway:
    no provider could stream: upstream error: 400 Bad Request:
    {"type":"error","error":{"type":"invalid_request_error",
    "message":"`temperature` is deprecated for this model."}}

because the platform's temperature setting was set, and the gateway sends
whatever it is given to whichever model the route names. It has no way to know
that this model rejects the parameter, that it takes an effort level where the
next one takes a thinking toggle, or that it will write 128k tokens of output
where the gateway asks for 4096. A route today is a protocol, a base URL, a
model name and a credential -- nothing about the model itself.

The facts exist, maintained by somebody else. [models.dev](https://models.dev)
is an MIT-licensed catalog of a few hundred providers and several thousand
models, served as one JSON document at `https://models.dev/api.json`, and its
entry for `claude-sonnet-5-5` says `"temperature": false`.

## What is taken from it, and what is not

Taken, per model:

| Field | Used for |
|---|---|
| `temperature` | Whether the parameter is sent at all |
| `reasoning`, `reasoning_options` | Mapping `reasoning_effort` onto a toggle, an effort level, or both |
| `tool_call` | Whether tools are offered |
| `attachment`, `modalities` | Which content parts a request may carry |
| `structured_output` | Whether a schema may be asked for |
| `limit.context`, `limit.output` | The context the trim works to, and `max_tokens` when a request names none |

**Not taken: `cost`.** outturn reports tokens, fuel and, in time, session
duration, and never money. What a token costs is the operator's business with
their provider, and what the operator charges is their business with their
workspaces; [usage.md](usage.md) already stores tokens and never prices for
that reason. A price in the catalog would be one more number for somebody to
mistake for the bill. The field is dropped when the catalog is stored, so
nothing downstream can come to rely on it.

## Nothing until somebody asks

No catalog ships with outturn. A copy in the image would be out of date on the
day it was built and further out every day after, describing models nobody can
buy and missing the ones they can, and a deployment would act on it without
anyone having chosen to. An empty catalog is honest: a route is written by hand,
as it is now, and the gateway sends what it is given, as it does now.

That also means a route never *needs* the catalog. A model served through
ollama or a vLLM -- `qwen3.5` locally -- will not be in it under that name, and
the form for a route works with every field typed in.

## Fetching

An operator presses a button on the Platform page. The gateway fetches the
document, since it is the tier that reaches the outside and the API makes no
outbound request of its own; the API stores what came back. One table, one row
per fetch:

```
model_catalogs (id, fetched_at, source_url, sha256, models jsonb)
```

`models` is the document reduced to the fields above, keyed by provider and
model id. The newest row is the catalog; older rows are kept, since routes
point into them (below).

Browsing is free once it is fetched. models.dev has no per-model endpoint --
the catalog is one document -- so there is nothing to browse before
downloading, and everything after is read from the stored copy as often as
anyone likes.

**Refreshing is limited by the server, not hidden by the UI.** The page shows
"Catalog from 12 October" and a refresh button, and the API refuses a second
fetch within a day, saying when the next one is allowed. That protects
models.dev as well as burying the button would, and an operator who needs a
model released this morning is not sent looking for a menu. Only a system
administrator may fetch.

## Routes point into it

A route gains three columns:

- `catalog_id` and `catalog_model` -- the stored catalog and the
  `provider/model` key the route was filled from. Null for a route typed by
  hand.
- `overrides jsonb` -- the fields somebody changed after choosing.

What the gateway acts on is the catalog entry with the overrides laid over it.
Choosing a model from the dropdown fills every field; changing one records it
as an override, marked as such on the form, with a plain note that a value
the model does not accept will fail at the provider rather than here.

A refresh moves every route still in the new catalog onto it, so a fact the
catalog corrected is corrected everywhere nobody overrode it. A route whose
model has left the catalog stays on the row it was filled from -- which is why
older rows are kept -- and the page lists it, since a model gone from the
catalog is usually a model about to be gone from the provider.

## What the gateway does with it

At the point it builds a provider request, with the route's resolved entry:

- `temperature: false` -- the parameter is left out, and the first such turn
  per route logs that the setting was ignored and why.
- `reasoning_options` -- `reasoning_effort` becomes the model's effort level,
  or its toggle, or both, instead of being dropped as the Anthropic
  translation drops it today.
- `limit.output` -- the default for `max_tokens`, replacing the 4096 in
  `src/gateway/llm/translate.rs`.
- `tool_call: false` -- a request carrying tools is refused before it is sent,
  with a message that says so, rather than sent to fail.

No entry, no change: the request is built exactly as it is today.

## Changing routes without a restart

Routes are already read on every request, so a row edited through the page
takes effect on the next turn. What does not is the credential.
`credential_ref` names an environment variable, read when the gateway starts,
so a route for a new provider is a manifest edit and a pod roll however easy
the page makes the rest.

So provider keys become [sealed credentials](sealed-credentials.md), held in
the platform workspace and bound to the provider's host. That document said
the gateway's own provider keys stay in the environment, because they changed
only when the operator changed provider, which was a deploy anyway. A page
that edits routes is what makes that untrue: changing provider stops being a
deploy. A sealed key is entered in the same page as the route that uses it,
and `credential_ref` keeps resolving environment variables for deployments
that already use them.

## Order of work

1. Say which provider rejected a request, and why, in one sentence rather
   than five nested ones. Independent of everything below, and the first
   thing anybody meets.
2. The route page, with routes typed by hand, keys sealed, and the explicit
   fall-back-to-the-built-in-provider row the roadmap describes. Useful on
   its own.
3. Fetching and storing the catalog, and browsing it.
4. Filling routes from it, with overrides.
5. The gateway acting on the resolved entry.

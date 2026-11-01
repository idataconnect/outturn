# File bodies

A request whose body is a stored file, sent without the file passing through
the tier that asked.

## Why

An agent could read a file's bytes (`read_object` with `format: "hex"`) but
could not send them anywhere: `fetch_url` took its body as a string. So any
API that takes an upload -- a document service, an image service, Tika's
`/meta` endpoint reading a photo's EXIF -- was out of reach, and wiring one up
meant a host import, which is a change to core every deployment carries. With
a file body it is a skill: an egress rule for the service and instructions
naming the call.

## How

The agent names a path: `fetch_url` with `body_file: "session/IMG_4815.HEIC"`
in place of `body`.

The runtime reads the file the way it reads any file: with its own storage
client, after the same read check (`object_at`). It stats the object for its
size and ETag, refuses one over `file_body::LIMIT` (32 MiB), and opens the
read with `If-Match` on that ETag, so the size it declares and the bytes it
sends are one object's: a file replaced in between fails the read before
anything is sent. It then sends the egress request to the gateway as a stream
rather than as JSON:

```
POST /v1/egress
Content-Type: application/vnd.outturn.egress-stream

[4 bytes: n, big-endian] [n bytes: the request as JSON] [size bytes: the file]
```

The JSON is the ordinary egress request (method, URL, headers, gates,
proof) with `body_size` in place of `body`. The handler reads the raw body:
JSON as before, bounded as before, and the stream only under its own content
type, with `n` checked against `file_body::HEAD_LIMIT` before anything is
allocated. The gateway reads the head whole, then vets the request exactly as
it vets any other -- rule, proof, address, credential, gates -- before it
reads a byte of the file. A head carrying `body_size` without the stream, or
the stream under the API's document-fetch token, is refused there.

Allowed, the rest of the stream becomes the outgoing request's body, with
`Content-Length: body_size`. The gateway passes on at most `body_size` bytes
and fails the send if the stream ends short; the runtime counts too and stops
if storage hands it more or less than it declared. Each chunk must arrive
within `file_body::IDLE`, and the outgoing request gets
`file_body::TIMEOUT` (ten minutes) in place of the ordinary thirty seconds,
since thirty seconds is a sustained 9 Mbit/s for a file at the limit.

The runtime charges what it holds in flight -- a chunk or two -- rather than
the file, since the file is never held.

So the bytes go from storage through the runtime and the gateway to the
recipient a chunk at a time: neither tier holds the file, nothing is
base64-encoded, and the gateway reaches nothing it did not reach before. The
size bound is the request's time allowance, not a pod's memory.

### A refusal before the file is read

The gateway answers a refused request without wanting the file. Between
these two tiers the connection is HTTP/1.1, and answering while the body is
still arriving makes the gateway's close a reset that often loses the answer
already sent -- a refusal the runtime would have to call did-not-complete. So
it reads and discards the rest first: at most `body_size` bytes, each within
`file_body::IDLE`. If the runtime stalls, it answers anyway and closes.

### A file that fails partway

The runtime stops the stream. The gateway's send fails mid-body, which the
transport already reports as possibly landed; a recipient honoring
`Content-Length` sees a short body and rejects it. Reported to the agent as
did-not-complete, as any request cut off mid-write is.

## Why not a link

The first version had the runtime presign a storage URL and the gateway read
it. It avoided the extra hop, but gave the gateway a path to the object store
and made it follow a URL the runtime wrote -- an SSRF boundary that would
have had to stay right forever. Streaming keeps both tiers inside what they
already reach and trust.

## Approvals

A file body on a request that needs approving is refused, not held. What a
person approves is what is sent (see *The approval carries the call* in
[approvals.md](approvals.md)), and a held file is a path whose contents can
change before anybody answers. The refusal is worded
unlike the one the runtime turns into an approval, so nobody is asked about
something that could never be sent as they saw it.

Holding one is the next step: keep the path and a SHA-256 of the bytes,
stream the file again when the approval comes, and have the gateway hash as
it streams and fail a send whose digest has changed.

## Not yet

- Skill operations (`skill-operations.md`) send a string body only. A file
  goes through `fetch_url`.
- What comes back is still a string, bounded as any response is.

## Recipe: a photo's metadata from Tika

A deployment running Tika (`--with tika`) can let agents ask it about a file:

1. Open Tika to the gateway: `OUTTURN_INTERNAL_HOSTS` on the API and the
   gateway names `tika:9998`, and a network policy lets the gateway reach it.
2. An egress rule for `tika:9998` in the workspace.
3. A skill telling the agent to `PUT http://tika:9998/meta` with
   `body_file` and an `Accept: application/json` header.

Tika identifies the type from the bytes, so there is no list of supported
types to keep. A type it has nothing to say about comes back with its
`Content-Type` and little else. A photo's metadata can include where it was
taken, to within metres, and anything the skill reads can end up in a reply.

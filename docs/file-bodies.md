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

The runtime checks the agent may read that path, exactly as for any read,
stats it, and refuses one over `file_body::LIMIT` (32 MiB). It then signs a
link that reads that one object for `LINK_SECONDS` (two minutes) and hands
the gateway the link and the size, never the bytes.

The gateway admits the link only if it points into the object store it was
configured with (`OUTTURN_S3_ENDPOINT` and `OUTTURN_S3_BUCKET`, its address
only, never its keys): same scheme, host and port, a path inside the bucket,
no user, no fragment, nothing percent-encoded that could walk out of the
bucket once the store resolves it. Then it vets the request as it vets any
other, opens the link, and streams the object into the outgoing request with
the stated `Content-Length`. If the store answers with a different length,
nothing is sent.

So the bytes go from storage to the recipient through the gateway alone: not
through the guest, not through the runtime's memory, and not base64-encoded
along the way. The size bound is the request's time allowance, not a pod's
memory.

## What it costs, and why it is safe

**The link is the runtime's to write.** That is the one thing here a
compromised runtime chooses, and without the store check the gateway would
read any URL it was handed from inside the cluster and send the answer to a
host an agent may reach -- a way past every egress rule there is. `Store::admit`
is that check, and `crates/gateway/src/egress/mod.rs`'s `file_bodies` tests
refuse each way around it before anything reaches the transport.

**The runtime signs something.** *The runtime signs nothing* in
[invariants.md](invariants.md) is about authority: it holds no key that could
mint a token for anyone. A presigned link is the same SigV4 signature the
runtime puts on every storage request it already makes, handed on for one
object, for reading, for two minutes. It grants nothing the runtime could not
already read, so a compromised one gains nothing by it.

**The gateway reaches the object store.** One more line in each network
policy (`k8s/base`, and `outturn-object-storage` in `k8s/deploy`). It holds no
storage credential, so it can read only what a valid link names.

## Approvals

A file body on a request that needs approving is refused, not held. What a
person approves is what is sent (see *The approval carries the call* in
[approvals.md](approvals.md)), and a held file is a link that lapses to a path
whose contents can change before anybody answers. The refusal is worded
unlike the one the runtime turns into an approval, so nobody is asked about
something that could never be sent as they saw it.

Holding one is the next step: keep the path and a SHA-256 of the bytes, sign
a fresh link when the approval comes, and have the gateway hash as it streams
and refuse a send whose digest has changed.

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

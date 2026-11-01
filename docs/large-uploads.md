# Large uploads

Getting big files, many files, and whole archives into outturn reliably.
Designed, not built.

## Why

Today a person uploads a file in one request, at most 25 MiB
(`files::MAX_UPLOAD_BYTES`), into a conversation's `session/`, `agent/` or
`workspace/` scope. That covers a photo or a contract. It does not cover what
people bring when they start using an agent in earnest:

- **One huge file.** A database export, a video, a year of scanned invoices
  in one PDF -- hundreds of megabytes to several gigabytes.
- **An archive.** Onboarding data as one zip: a shared drive, an old system's
  export, a folder of policies.
- **A pile of documents.** A folder dropped on the page, hundreds of files of
  every size, some of which fail and have to be sent again.

Each needs the same three things: a transfer that survives a dropped
connection, a way of knowing what arrived, and nothing in the platform holding
a whole file in memory.

## Pieces, tracked by the store

A large file goes up as an S3 multipart upload: the store gives it an id,
takes numbered pieces in any order, and joins them when told they are all
there. Every S3-compatible store does this, including the MinIO this ships, so
it is not a choice an operator has to make.

The store is what tracks the pieces. Asked, it lists the ones it already has,
so resuming is a question to the store rather than a table of chunks here that
could disagree with it. Pieces nobody finished are swept by the bucket's
lifecycle rule a day after the upload began (`S3Storage::ensure_lifecycle`),
which already exists for the streaming writers.

What the platform keeps is one row per upload:

| Field | Is |
|---|---|
| `id` | The upload, as the browser names it |
| `workspace_id`, `user_id` | Whose it is |
| `scope`, `path` | Where it lands: the same scopes and paths a single upload takes |
| `size` | What the person said it would be, so the quota is checked first |
| `store_upload_id` | The store's id for the multipart upload |
| `state` | `open`, `complete` or `abandoned` |
| `created_at`, `expires_at` | A day, matching the lifecycle rule |

## Through the API, by default

The browser sends each piece to the API, and the API passes it to the store as
that piece of the multipart upload. Pieces are 16 MiB, so the API holds at
most one per request and the store's 5 MiB minimum piece size is met.

The alternative -- the browser sending pieces straight to the store through
links the API signs -- is faster and keeps the bytes off the API entirely. It
is not the default, because it needs the store to be reachable from the
person's browser and configured for cross-origin requests. The MinIO this
ships is inside the cluster, so a turnkey deployment would fail out of the
box. It is a later opt-in for an operator whose storage is public. The upload
row and the resume logic are the same either way; only the transport differs.

## The calls

```
POST   /v1/agent-sessions/{id}/uploads                   {scope, path, size}
         -> {upload_id, piece_size}
PUT    /v1/agent-sessions/{id}/uploads/{upload_id}/{n}   (one piece's bytes)
GET    /v1/agent-sessions/{id}/uploads/{upload_id}        -> {received: [n...]}
POST   /v1/agent-sessions/{id}/uploads/{upload_id}/complete
DELETE /v1/agent-sessions/{id}/uploads/{upload_id}
```

- **Start** checks the scope authority and path exactly as a single upload
  does, and refuses a size the workspace's storage quota cannot take, before
  a byte is sent.
- **A piece** is checked against the declared size: no piece past the end,
  and only the last may be short.
- **Status** asks the store which pieces it has. A browser that lost its tab
  calls this and sends the rest.
- **Complete** has the store join the pieces, checks the result is the
  declared size, and then does what a single upload does once a file has
  landed: the HEIC copy, extraction for search, the file showing up for the
  agent. A file replaced at the same path is replaced only now, when the new
  one is whole.
- **Abort** tells the store to drop the pieces.

A file under the piece size still goes up in one request, as today.

## Archives

A zip uploaded this way can be unpacked into a scope as a job of its own
rather than inside an agent's turn. A turn's unpacking (`expand_archive`)
holds the turn open, counts against its time, and is capped at 10,000
entries of 32 MiB each, which is right for an agent tidying a download and
wrong for onboarding a company's files.

The job runs on the API's worker, reading the archive from the store and
writing each entry with the streaming writers, so no entry is held whole. It
ends in a report a person can read and the agent is told: how many files
landed and where, and each one skipped and why -- too large, a name that
would escape the destination, a type that is refused. Each file is then
extracted for search like any other upload.

The person chooses where it lands. Onboarding data belongs in `agent/`, kept
and shared by every conversation with that agent. `session/` is scratch,
swept when the conversation has been quiet for
`OUTTURN_SESSION_FILE_TTL_DAYS`.

## Many files at once

The page takes a folder or many files and queues them: a few transfers at a
time, each retried on its own with backoff, and a list showing each one as
waiting, sending, done or failed, with a retry for the failed. Small files go
in one request and large ones in pieces, and the person never chooses.

A file whose contents are already at its path is skipped rather than sent
again, judged by size and a hash the browser computes as it reads, so dropping
the same folder twice is harmless.

## When a file is usable

A file is usable when it is stored and read. Until then the Files tab says so
beside it, and an agent that asks for it is told it is still being read, as
happens today for a document Tika has not finished.

## Not here

- **Direct to storage.** The opt-in above, once an operator needs it.
- **Resuming across days.** The lifecycle rule's day bounds how long an
  upload can be left half done. Longer means a longer rule, which costs the
  store whatever pieces sit in it.
- **Formats other than zip.** A tarball or 7z unpacks the same way once
  there is a reader for it; zip covers what people are sent.

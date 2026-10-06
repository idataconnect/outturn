# Streaming storage

How a guest writes and reads objects of any size without either tier holding
the whole thing. A design; nothing here is built yet.

## Why

Customers will want agents that produce files -- PDFs, spreadsheets, archives,
images -- and keep them in their bucket. Storage today was shaped around text a
model wrote, and it shows in three places:

- **Writes are whole-object.** `write-object` takes the entire file as one
  `list<u8>`. It sits in the guest's linear memory, is copied into a host
  `Vec<u8>`, and goes up in a single `put_object`. A file is limited by the
  smaller of the guest's 128 MiB and whatever the pod can spare a second time.
- **A write at an offset rewrites the object.** `S3Storage::write` with
  `offset != 0` fetches the whole existing object, splices the new bytes in,
  and puts all of it back. Writing a file in chunks this way moves O(n²) bytes
  and holds the whole object in host memory on every call. Nothing calls it
  with a nonzero offset today, which is the only reason it has not hurt.
- **The tool a model calls is text-only.** `write_object` takes a JSON string
  and stores its UTF-8 bytes, so a model cannot hand it anything that is not
  text.

## Why not a filesystem

The sandbox has none: WASI's filesystem interfaces are linked, but the context
is built with no preopened directories. Giving it one -- so a guest could
generate into a file and the host upload it afterwards -- is the obvious move
and the wrong one.

A virtual filesystem in memory is guest-sized data held outside the guest's
cap, where admission cannot see it. A real directory spends pod ephemeral
storage, which is a new resource to set quotas on and clean up after a turn
that crashed, and kubelet evicts a pod whose ephemeral storage fills -- the
same outcome as an OOM kill, for every turn on it. And neither streams: the
whole file exists somewhere before its first byte reaches the bucket.

## Writers

```wit
/// An object being written. Nothing exists at the path until `finish`.
resource object-writer {
    /// Appends. The host uploads a part whenever it holds enough for one.
    write: func(chunk: list<u8>) -> result<_, string>;
    /// Completes the object and says what was stored.
    finish: func() -> result<object-info, string>;
}

/// Opens a writer, under the same scope rules and refusals as `write-object`.
open-writer: func(path: string) -> result<object-writer, string>;
```

On S3 a writer is a multipart upload. The host buffers chunks until it holds a
part -- 5 MiB, S3's minimum for any part but the last -- uploads it, and
starts the next. `finish` uploads what remains as the last part and completes
the upload. A file smaller than one part never starts a multipart upload: it
is buffered and sent with `put_object` at `finish`, which is one request
instead of three.

Host memory per open writer is one part, whatever the size of the file. The
guest holds only the chunk it is passing.

A writer is checked against the scope when it opens, not when it finishes, so
a guest is refused before it has spent a turn generating something it may not
keep. `finish` then does what a write does today: invalidates any extraction
of an older object at that path and notifies `on_write`. Those steps move into
one helper that `write-object`, `finish` and anything else that writes all
call, so no path into storage skips one.

### Atomic by construction

An object a multipart upload never completed does not exist. So a writer
dropped without `finish` -- by the guest, by a trap, by the turn being
cancelled or its pod dying -- leaves nothing behind at the path, and whatever
was there before is still there. The host aborts the upload when the resource
is dropped. An abort that never runs, because the pod died first, leaves
parts that are invisible but billed, so the bucket also gets a lifecycle rule
that aborts incomplete multipart uploads after a day, installed at startup
beside the session sweep.

This is the property that matters most: a reader never sees half a file, and
a failed write is never mistaken for a short one.

### Admission

An open writer is charged to admission for one part, through a
`try_charge(bytes)` that a running turn uses for anything it is about to hold
on top of its flat charge. The guard comes back as part of the writer and the
bytes return when it drops. The check is `try_admit`'s: refused when free
memory less what is already spoken for cannot cover the reserve and the
charge, and never refused when this is the only turn on the pod, for the same
reason an idle pod always takes a turn.

A refusal is an error from `open-writer`, not a failed turn. The agent can
wait and retry, or say so.

The number of writers one turn may hold open is capped, small -- four -- so
the charge stays something admission can reason about.

## Readers

```wit
/// An object being read, from the front.
resource object-reader {
    /// Up to `len` bytes; empty once the object is exhausted.
    read: func(len: u32) -> result<list<u8>, string>;
}

/// Opens a reader on an object's stored bytes.
open-reader: func(path: string) -> result<object-reader, string>;
```

Backed by `get_object_stream`: one request, consumed as the guest asks. It
replaces the case where `read-bytes` is asked for everything and pulls the
whole object into a `Vec`. Ranged `read-bytes` stays, for a guest that wants a
piece from the middle. `read-object`, which returns extracted text, is
unchanged.

## Offsets

`write` at a nonzero offset goes. Nothing calls it, writers cover the use it
was reaching for, and keeping it means keeping a code path that reads a whole
object in order to change part of it. `StorageBackend::write` loses its offset
argument.

## What the model sees

`write_object` gains `content_base64`, exclusive with `content`, for small
binaries a model produces itself -- an icon, a short CSV it wants stored
byte-exact. It is capped (1 MiB decoded) because base64 in a tool call is the
most expensive way there is to move bytes: every one of them is generated as
output tokens.

Anything larger is not something a model should be emitting at all. It comes
from code -- a renderer, an export, an archive -- and that code uses writers
directly. Rendering a PDF is the first such case: the model writes markdown,
the host renders it, and the result goes to storage through a writer without
passing through the guest. See the PDF rendering design when it lands.

## Not covered

Copying an object, appending to an existing one, and resuming an upload after
a turn fails. Each is possible on multipart -- `UploadPartCopy`, a fresh
upload seeded from the old object, keeping the upload ID across attempts --
and none is needed by anything yet.

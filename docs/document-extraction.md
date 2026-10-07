# Document extraction

A PDF is bytes a model can do nothing with. Where Apache Tika is deployed,
documents are read into text once, the text is stored beside the object, and
an agent that reads the document is handed the words.

## Turning it on

`OUTTURN_TIKA_URL` is the switch, and it is set on two tiers:

| Tier | Reads it for |
|---|---|
| `api` | Where to send documents. Unset, no extraction job is ever enqueued and the worker does not start |
| `runtime` | Only whether extraction exists. The runtime never calls Tika |

The `tika` component (`scripts/dev.sh --with tika`, or
`k8s/components/tika`) deploys Tika and sets the variable on both, so the
service and its configuration arrive together or not at all. Set them
together by hand too: a runtime told extraction exists when the API is not
running it answers "still being read" for a job that will never run.

## What happens

- **Extractable** means the extension: PDF, Word, OpenDocument, RTF, Excel,
  PowerPoint, EPUB, `.msg` and `.eml`. Anything else is read as bytes.
- **On every write**, by a person uploading or an agent writing, a background
  job is queued, serialized on the object's key. It runs in the API tier, where
  the bucket and the database already are, so a scanned document taking
  minutes holds no request open.
- **The text** is stored at `extracted/<object key>`, outside the scope
  prefixes so a listing never shows it beside its document. A write or delete
  of the object removes it.
- **A permanent failure** -- the job's last attempt failing -- is
  written to `extracted/<object key>.failed`, so "never" reads differently from
  "not yet".
- **Files stored before extraction was configured** are queued when a listing
  first finds them without text, after the listing has answered.

An agent reading a document gets its text, its stat reports the text's size,
and it is told plainly when the document is still being read, could not be
read, or held no text at all -- a scan, usually. Nothing reads text out of an
image; see [vision.md](vision.md).

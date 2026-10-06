# PDF rendering

How an agent makes a PDF, why the renderer is a service of its own, and where
it goes next. Built.

## What exists

An agent calls `render_pdf` with markdown and a path. The runtime sends the
markdown to the renderer service, stores the PDF that comes back, and tells
the agent the path and size; the document never passes through the guest. Headings,
paragraphs, bold, italic, inline code, links (clickable), ordered, unordered
and task lists, block quotes, code blocks, rules and tables with column
alignment are laid out on A4 with page numbers, and the first top-level
heading becomes the document's title. Images are not drawn yet and appear as
their alt text.

The renderer is `renderers/pdf`: markdown parsed by `pulldown-cmark`, laid out
by a flow layout written here, and written out by
[krilla](https://github.com/LaurenzV/krilla) -- the PDF writer under Typst,
without the typesetting engine above it, which is what gets font subsetting
and embedding right. Four DejaVu faces are built in (regular, bold,
oblique, mono). They cover Latin, Greek and Cyrillic and a good deal else, but
not CJK; a document in a script the fonts lack renders its missing glyphs as
boxes.

Emoji are drawn in color from Twemoji, a fifth face that takes any character
the text faces lack and it has -- or one followed by the emoji variation
selector, which asks to be an emoji. Joiners, skin tones, keycaps and flag
pairs stay with the emoji they modify, so a family or a flag is shaped as the
one glyph it is. Its glyphs are PNG bitmaps, embedded as images. It was added
after the first real document an agent made ended in three empty boxes: a
model reaches for emoji without being asked, and a box where one should be
reads as broken. Attribution for both font families is in `NOTICE`.

## Why a service of its own

It began inside the runtime, as a WASM component the host instantiated per
call with its own memory cap and fuel. That made every runtime pay for it --
a 9 MB component in every image, compiled at every pod's startup and resident
in pods sized for turns -- whether or not the deployment ever made a PDF. And
once the renderer was out of the agent's sandbox, the sandbox around it was
guarding very little: it holds no credentials and reaches nothing, so there was
nothing for WASM to protect but the pod's own time.

So it is a service, deployed as Tika is: an optional component
(`k8s/components/pdf-renderer`, `scripts/dev.sh --with pdf-renderer`) with its
own image, found through `OUTTURN_PDF_RENDERER_URL` on the runtime. Unset, no
call is made and `render_pdf` answers that rendering is not available here. The
runtime image carries none of it.

The service holds nothing and calls nothing -- no database, no credentials, no
bucket, and a network policy with no egress. Its only exposure is time, which
it bounds per pod:

| | Value | Why |
|---|---|---|
| Markdown | 2 MiB at most | Several hundred pages; refused before it is rendered |
| At once | 2 | Rendering is all CPU; a third is told the renderer is busy |
| Time | 30 s per render | The largest input takes about 4 s |
| Pod | 256 Mi, 2 CPU | The largest input peaks near 14 MB |

The time limit bounds how long a caller waits, not the work: a render already
running cannot be stopped from outside its thread, so a pathological one keeps
its slot until it is done. The size cap is what bounds how long that is. What
WASM gave that this does not -- a cap on each render's memory rather than the
pod's -- was not worth carrying for that.

Every refusal is a sentence the agent can act on -- too large, busy, too slow,
not available here -- returned as an ordinary tool error, and the turn carries
on.

## Two passes

Each page says "n / N", so N has to be known before page 1 is drawn. The first
pass lays out the document and only counts pages; the second lays out again
and draws each page as it is finished, then drops it. Layout depends on
nothing but the markdown and the fonts, so both passes break pages in the same
places, and laying out is cheap beside drawing.

The alternative -- lay out everything, then draw -- held every word of every
page at once. Single-pass writers like iText avoid that by drawing totals into
a placeholder filled at the end, whose width has to be guessed because the
page around it is already written; that is why their "Page x of y" so often
sat oddly. Two passes measure the real string.

Within a line, words that share a style are drawn as one piece, spaces
included. Each draw shapes its text afresh, and drawing word by word made the
largest input several times slower.

What still grows with the document is the PDF itself: krilla holds what it has
written and subsets fonts at the end, when it knows every glyph used. About
2 KiB a page. A writer that streamed each finished page into an object writer
would make it flat -- the format allows it; the offsets in the cross-reference
table are of bytes already written -- but that means `pdf-writer` and font
subsetting done here, and nothing the cap admits needs it yet.

## Where it goes next

**Queued rendering.** Large or many documents -- a month-end run of a few
hundred reports -- are better as jobs than as calls an agent waits on: queued,
rendered by the same service, and the session woken when the file is there, as
`sleep` and `set-timer` do. It costs a queue wait, and a woken turn may find
the provider's prompt cache cold. Not a model round: rendering in place is
followed by one too.

`render-pdf` is a host import, so the swap is the host's decision per call:
render now and return the file, or queue it and tell the guest when it will be
woken. A guest's code does not change.

**A better default look.** A document looks professional because its design
is good, not because its source language is rich: given more to write, a model
mostly writes the same headings and tables. So the next gain is in the
renderer -- typography, a title block and running header, and a few named
themes a model chooses between -- with an accent color and inline color for
the figure that has to stand out. Every model gets the same good output,
because the design lives here rather than in the prompt.

**Images.** Read from storage by the runtime and sent with the markdown,
decoded in the renderer, each capped in pixels before it is decoded -- a
decoded image is the one thing that could change the memory picture above.

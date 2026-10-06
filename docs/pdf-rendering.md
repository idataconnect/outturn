# PDF rendering

How an agent makes a PDF, why it is a component of its own rather than part of
the agent, and where it goes next. Built.

## What exists

An agent calls `render_pdf` with markdown and a path. The host renders it and
stores the result; the document never passes through the guest. Headings,
paragraphs, bold, italic, inline code, links (clickable), ordered, unordered
and task lists, block quotes, code blocks, rules and tables with column
alignment are laid out on A4 with page numbers, and the first top-level
heading becomes the document's title. Images are not drawn yet and appear as
their alt text.

The renderer is `renderers/pdf`: markdown parsed by `pulldown-cmark`, laid out
by a flow layout written here, and written by
[krilla](https://github.com/LaurenzV/krilla) -- the PDF writer under Typst,
without the typesetting engine above it, which is what gets font subsetting
and embedding right. Four DejaVu faces are compiled in (regular, bold,
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

## Why a component of its own

Every turn's guest is capped at 128 MiB, and admission charges each turn a
flat 100 MiB while allowing eight at once on a 512 Mi pod. That only works
because a guest sits well below its cap. A PDF library linked into the agent
would make every agent larger, and a guest that actually rendered would spend
the headroom the flat charge is counting on.

So the renderer is instantiated by the host per call, in a store of its own:

| | Value | Why |
|---|---|---|
| Memory cap | 32 MiB | The largest input peaks at 14.1 MiB, emoji throughout |
| Fuel | 150 G, its own | The largest input spends 50 G. A runaway guard, not a budget |
| Markdown | 2 MiB at most | Several hundred pages; refused before rendering past it |
| At once, per pod | 2 | Rendering is all CPU |

While a render runs it is charged to admission (`try_charge`) for its cap and
the document it hands back. Refused when the pod is short of memory -- unless
the turn is the only one on the pod -- and the refusal is an ordinary tool
error the agent can wait out, not a failed turn. A trap inside the renderer is
the same: "too large or too complex to render", and the turn carries on.

The renderer imports nothing the host gives meaning to. It is built for
wasip2, so the standard library links WASI, and the host gives it a context
with no directories, environment or network. Fonts are compiled in and the
document is the return value. A compromised renderer can return bad bytes.

It is compiled the first time it is wanted, and a runtime compiles it at
startup before it reports ready: a renderer that cannot be used is a broken
build, found there rather than by the first agent to ask.

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
included. Each draw shapes its text afresh, and drawing word by word is what
first took the largest input past its fuel.

What still grows with the document is the PDF itself: krilla holds what it has
written and subsets fonts at the end, when it knows every glyph used. About
2 KiB a page. A writer that streamed each finished page into an object writer
would make it flat -- the format allows it; the offsets in the cross-reference
table are of bytes already written -- but that means `pdf-writer` and font
subsetting done here, and nothing the cap admits needs it yet.

## Where it goes next

**Queued rendering.** Large or many documents -- a month-end run of a few
hundred reports -- belong on workers of their own, pulling render jobs as
runtimes pull turns, and waking the session when the file is there, as
`sleep` and `set-timer` do. The workers scale on their own queue, hold no turn
memory, and can afford a far larger cap: that is where Typst, or the streaming
writer above, would live. It costs a queue wait, and a woken turn may find the
provider's prompt cache cold. Not a model round: rendering in place is
followed by one too.

`render-pdf` is a host import, so the swap is the host's decision per call:
render now and return the file, or queue it and tell the guest when it will be
woken. A guest's code does not change.

**Compute in the ledger.** The host knows exactly what each render spent --
`Rendered::fuel` -- and records it nowhere, which is true of a turn's own fuel
too. See the roadmap. Fuel is the right unit to meter (deterministic, counted
outside anything the guest can address) and the wrong one to budget: a
customer meets a price, and the per-call figure above stays a runaway guard.

**Images.** Read from storage by path, decoded inside the renderer, each
capped in pixels before it is decoded -- a decoded image is the one thing that
could change the memory picture above.

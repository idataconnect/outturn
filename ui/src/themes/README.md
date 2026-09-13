# Themes

A deployment's look, in one file each.

outturn is Apache-2.0 and expects to be deployed by companies serving their
own customers under their own name. Skinning it must therefore not mean
forking it: a deployer who edits `index.css` and the components has a branch
that fights every upgrade, which is how an open source platform loses the
people running it.

So everything a brand touches is a token, and a theme is the file that sets
those tokens. `outturn.css` is the default. `hollowbrook.css` is a second,
deliberately unlike it -- a bed and breakfast, warm and serif where outturn is
cool and sans -- which exists so that "the tokens are complete" is something
a test checks rather than something someone claimed once.

## Adding one

Copy `hollowbrook.css`, change the values, and set `VITE_THEME` to its name.
Every token the default defines should be set; anything left out falls back
to outturn's value and will look like outturn by accident.

Colours are OKLCH so a ramp can be reasoned about rather than eyeballed: hold
lightness and chroma, move hue, and the new scale stays as legible as the one
it came from.

## What a theme may set

| Token | What it colours |
|---|---|
| `--color-brand-50..950` | Interactive elements: links, primary buttons, the active nav item |
| `--color-surface-50..950` | Every flat surface, and text on them |
| `--color-surface-875` | The page a conversation sits on, a half-step from 900 |
| `--color-accent-400..600` | Decoration that is not a control, such as the avatar gradient |
| `--font-sans`, `--font-display` | Body text, and the wordmark |

Branding that is not colour -- the product name, the logo -- comes from
`src/lib/brand.ts`, which reads the same environment.

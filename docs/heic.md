# HEIC photos

An iPhone saves photos as HEIC. That is what arrives when somebody uploads a
photo straight off a phone, or drags one out of a Mac's photo library -- the
first upload that showed this was a `.heic` dragged from Finder, which no
model on the route could read, and which failed later, inside a turn, in a
way the person who uploaded it could do nothing about.

Few providers read HEIC, and no browser but Safari shows it. So it is
converted once, at upload, and nothing after that ever sees it.

## What happens at upload

The API sniffs the bytes, not the name: a HEIF file says so in its first
twelve bytes, and a phone can send one called `image.jpg`.

- **A converter is deployed** (`OUTTURN_HEIC_URL` set): the photo is sent to
  it, and the JPEG that comes back is stored beside the original under the
  same name ending `.jpg` -- `IMG_1234.HEIC` and `IMG_1234.jpg`. The upload
  answers with the JPEG's path, so the JPEG is what the composer mentions to
  the agent and what `describe-image` sends a model. The JPEG keeps the
  photo's rotation and is averaged down to 2048 pixels on its longest side,
  which is past what a vision model looks at and enough for a browser.
- **None is deployed**: the upload is refused with 415, and says that HEIC
  photos are not converted here and that a JPEG or PNG can be uploaded
  instead. Nothing is stored.
- **The conversion fails**: the upload is refused with 422 and the
  converter's reason. Nothing is stored.

## Why a component

`k8s/components/heic`, turned on with `scripts/dev.sh --with heic`. Not part
of any core image, for three reasons:

- **Size.** libheif, its HEVC decoder and what they link come to about 4MB,
  in every runtime of every deployment, for a feature most never use. The
  converter's own image is a Debian base rather than distroless, because
  libheif loads its decoder as a plugin and the package manager is what puts
  that where it looks.
- **Licensing.** HEVC is patent-encumbered, and the obligation tends to fall
  on whoever distributes the decoder. In an image of its own, that is a
  choice made by the operator who turns it on -- and so the release does not
  publish it beside the core images. A deployment that wants it builds it,
  `docker build --target heic .`, and pushes it where its cluster pulls from.
- **Untrusted input.** A video decoder written in C, handed whatever anybody
  uploads, belongs in a pod with nothing worth reaching. Like Tika, the
  converter holds no credentials, storage or database; only the API may reach
  it, and it may reach nothing. Its service bounds what one request can
  spend: 25MB in, 50 megapixels decoded, two at once, thirty seconds each
  (`renderers/heic/src/service.rs`).

The crate is a workspace of its own, so building and testing the rest of
outturn never needs libheif. Working on it does: `libheif-dev` and
`libheif-plugin-libde265` on Debian and Ubuntu, `libheif` on Arch and
Homebrew.

## What is not built

- **A file an agent writes is not converted.** Only uploads are. An agent
  that fetched a HEIC from somewhere has a file `describe-image` refuses.

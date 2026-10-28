# HEIC photos

Phones save photos as HEIC. That is what arrives when somebody uploads a
photo straight off a phone, or drags one out of a desktop photo library -- the
first upload that showed this was a `.heic` dragged from a desktop file
manager, which no model on the route could read, and which failed later,
inside a turn, in a way the person who uploaded it could do nothing about.

Few providers read HEIC, and only one browser shows it. So it is
converted once, at upload, and nothing after that ever sees it.

## What happens at upload

The API sniffs the bytes, not the name: a HEIF file lists its brands in its
first box, and a phone can send one called `image.jpg`. A file naming an HEVC
brand is HEIC; one naming only the generic HEIF brands is too, unless it also
says AV1 -- that is an AVIF, which is not this converter's, and is stored as
it came.

- **A converter is deployed** (`OUTTURN_HEIC_URL` set): the photo is sent to
  it, and both files are stored, each named for what it is:
  - the original at its own name if that ends `.heic` or `.heif`, replacing
    whatever was there as any upload does; otherwise as `<name>.heic`, so a
    HEIC called `photo.jpg` is not kept under a JPEG's name.
  - the JPEG as `<name>.jpg`, or `<name>-2.jpg` and on if that is taken. A
    JPEG made here never replaces a file somebody put there; uploading the
    same photo twice leaves two JPEGs.

  The upload answers with the JPEG's path and `converted_from`, the
  original's, so the JPEG is what the composer mentions to the agent and what
  `describe-image` sends a model. The JPEG keeps the photo's rotation and is
  averaged down to 2048 pixels on its longest side, which is past what a
  vision model looks at and enough for a browser. The JPEG is written first;
  if the original then cannot be, the JPEG is removed, so an upload stores
  both or neither.
- **None is deployed**: the upload is refused with 415, and says that HEIC
  photos are not converted here and that a JPEG or PNG can be uploaded
  instead. Nothing is stored.
- **The converter is down, busy or slow**: 503, saying to try again shortly.
  Nothing is stored, and the photo is not called broken.
- **The photo cannot be converted**: 422, with the converter's reason, cut to
  300 characters. Nothing is stored.

The converter's reply is not trusted any more than the photo was: it is read
up to 25MB and no further, accepted only if it starts as a JPEG does, and an
error page from anything between the two -- a proxy, a pod restarting -- is
said as the converter being unavailable rather than shown as it came.

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
  (`renderers/heic/src/service.rs`). Two at the pixel bound peak near 690MB,
  measured, which is what the pod's 1Gi limit is for.

`k8s/deploy` does not open a path to it. A deployment that turns it on there
needs `k8s/deploy/examples/document-services-networkpolicy.yaml`, or the
namespace's default deny answers every HEIC upload with "the photo converter
is not answering"; see [deployment.md](deployment.md).

The crate is a workspace of its own, so building and testing the rest of
outturn never needs libheif. Working on it does: `libheif-dev`,
`libheif-plugin-libde265` and `zlib1g-dev` on Debian and Ubuntu, `libheif` on Arch and
Homebrew.

## What is not built

- **A file an agent writes is not converted.** Only uploads are. An agent
  that fetched a HEIC from somewhere has a file `describe-image` refuses.

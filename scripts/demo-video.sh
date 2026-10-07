#!/usr/bin/env bash
# Records the README's walkthrough and encodes it: docs/demo.mp4, and
# docs/demo.gif for where a video will not play.
#
# Against a workspace from scripts/demo-seed.sh, with the UI's dev server up.
# The walkthrough approves the seeded charge, so a second take needs a fresh
# seed -- or a dump taken after seeding, restored into an empty schema.
#
#     scripts/demo-video.sh
set -euo pipefail

cd "$(dirname "$0")/.."
eval "$(scripts/dev-secrets.sh --print 2>/dev/null | grep -E '^OUTTURN_DEV_ADMIN_(EMAIL|PASSWORD)=' | sed 's/^/export /')"

out=$(mktemp -d)
trap 'rm -rf "$out"' EXIT
(cd ui && OUT="$out" node e2e/demo-video.mjs >/dev/null)
video=$(jq -r .video "$out/marks.json")
start=$(jq -r '.marks[] | select(.name == "dashboard") | .at' "$out/marks.json")

# From the first frame of the dashboard: before it is a blank page loading.
ffmpeg -loglevel error -y -ss "$start" -i "$video" \
  -c:v libx264 -preset slow -crf 24 -pix_fmt yuv420p -movflags +faststart -an \
  docs/demo.mp4

# A GIF has 256 colors to the frame, so it is given a palette made from this
# video, and is smaller and slower than the MP4 to keep it a size a README
# can carry.
ffmpeg -loglevel error -y -i docs/demo.mp4 \
  -vf 'fps=10,scale=960:-1:flags=lanczos,split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle' \
  docs/demo.gif

ls -la docs/demo.mp4 docs/demo.gif

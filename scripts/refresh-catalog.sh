#!/usr/bin/env bash
# Rebuilds the model catalog this repository ships (assets/catalog.json.gz)
# from models.dev, reduced to the fields outturn reads. Run before a release,
# or whenever the shipped catalog should know about newer models; commit the
# result. A deployment refreshes its own copy on its own schedule -- this only
# moves the floor it starts from.
set -euo pipefail
cd "$(dirname "$0")/.."

source="${OUTTURN_CATALOG_URL:-https://models.dev/api.json}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

curl -fsSL "$source" -o "$tmp/catalog.json"
cargo run --quiet --bin outturn-catalog -- "$tmp/catalog.json" assets/catalog.json.gz

#!/usr/bin/env bash
# What counts as a release, for the two workflows that have to agree on it:
# images.yml, which publishes a release tag's images under its version, and
# image-cleanup.yml, which never deletes a version carrying one. Written once
# so the second cannot keep a different set from the one the first wrote.
#
#   release-version.sh ref <ref>     the version a pushed ref publishes: empty
#                                    for main, 1.2.0 for v1.2.0, and failing
#                                    for anything else
#   release-version.sh tags <a,b,c>  succeeds if any of an image's tags is a
#                                    release version, with or without the v
#
# A version is three numbers and an optional suffix, as in 1.2.0-rc.1.
set -euo pipefail

version='[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?'

case "${1:-}" in
  ref)
    ref="${2?a ref}"
    if [[ "$ref" == main ]]; then
      echo ""
    elif [[ "$ref" =~ ^v($version)$ ]]; then
      echo "${BASH_REMATCH[1]}"
    else
      echo "not main and not a release tag: $ref" >&2
      exit 1
    fi
    ;;
  tags)
    [[ "${2?a tag list}" =~ (^|,)v?$version(,|$) ]]
    ;;
  *)
    echo "usage: $0 ref <ref> | tags <tag,...>" >&2
    exit 2
    ;;
esac

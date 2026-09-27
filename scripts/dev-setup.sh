#!/usr/bin/env bash
# Asks this machine's model settings, once, into a gitignored file.
#
#   scripts/dev-setup.sh                 # ask if there are no answers yet, otherwise say nothing
#   scripts/dev-setup.sh --reconfigure   # ask again, the current answers as the suggestions
#   scripts/dev-setup.sh --show          # where the answers are, and what they say
#
# scripts/dev.sh and scripts/dev-mac.sh run this for you (and take
# --reconfigure too); running it alone is for changing an answer without
# starting the loop. What each answer drives is in docs/local-development.md.
set -euo pipefail

cd "$(dirname "$0")/.."
source scripts/lib/dev.sh

reconfigure=false
show=false
for arg in "$@"; do
  case "$arg" in
    --reconfigure) reconfigure=true ;;
    --show) show=true ;;
    *)
      echo "unknown argument: $arg" >&2
      exit 2
      ;;
  esac
done

file=$(dev_machine_file)

if [[ "$show" == true ]]; then
  dev_machine_load
  if [[ -f "$file" ]]; then
    echo "$file"
  else
    echo "no answers yet; these are the suggestions (${dev_hint:-nothing detected})"
  fi
  echo "  model           $dev_model"
  echo "  served as       $dev_served_model"
  echo "  context window  $dev_context_window tokens"
  echo "  context budget  $dev_context_budget bytes"
  echo "  ollama          ${dev_ollama_url:-(not ollama)}"
  exit 0
fi

dev_machine_setup "$reconfigure"

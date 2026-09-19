#!/usr/bin/env bash
# Prepares a production deployment: asks what it cannot know, generates what
# nobody should choose by hand, and refuses what would be unsafe.
#
#   scripts/deploy-prod.sh                  # ask, generate, write, check
#   scripts/deploy-prod.sh --check-only     # run the checks against an existing overlay
#   scripts/deploy-prod.sh --dir DIR        # somewhere other than k8s/overlays/prod
#
# Writes manifests and stops. Applying them is left to you: which cluster
# receives a production deployment is a decision, and a script that makes it
# quietly is a script that makes the wrong one. The last thing printed is the
# command to run.
set -euo pipefail

cd "$(dirname "$0")/.."
# shellcheck source=lib/keys.sh
source scripts/lib/keys.sh

dir=k8s/overlays/prod
check_only=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    --check-only) check_only=true ;;
    --dir)
      dir="${2:?--dir needs a path}"
      shift
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
  shift
done

secret_file="$dir/secret.yaml"
fail=0

note() { printf '  %s\n' "$*" >&2; }
bad() {
  printf '  REFUSED  %s\n' "$*" >&2
  fail=1
}

# ---------------------------------------------------------------------------
# Checks. Run after writing as well as alone, so --check-only and a fresh
# generation answer the same question about the same files.
# ---------------------------------------------------------------------------
check() {
  echo "checking $dir" >&2

  if [[ ! -f "$secret_file" ]]; then
    bad "no $secret_file: run without --check-only to generate one"
    return
  fi

  # The dev seed creates a system admin with a known email on any empty
  # database. Harmless locally, an open door anywhere reachable.
  if grep -qE '^\s*(-\s*name:\s*)?OUTTURN_DEV_SEED' "$dir"/*.yaml 2>/dev/null; then
    bad "OUTTURN_DEV_SEED appears in $dir: it seeds an admin account with a known password"
  else
    note "OK       no dev seed"
  fi

  # Anything this repository has ever committed is public, whatever it is
  # named now and wherever it was copied from.
  local published=0 value
  while IFS= read -r value; do
    if outturn_is_published_key "$value"; then
      bad "a key in $dir is one this repository published; generate a new one"
      published=1
      break
    fi
  done < <(grep -hoE '[A-Za-z0-9/+=_-]{16,}' "$dir"/*.yaml 2>/dev/null || true)
  [[ "$published" == 0 ]] && note "OK       no published dev key"

  # The signing secret and the public key have to belong to each other. A
  # mismatched pair starts three healthy-looking tiers that reject each
  # other's tokens -- the failure src/auth/token.rs:149 records having been
  # through once already.
  local secret public derived
  secret=$(awk '/OUTTURN_TOKEN_SECRET:/ { print $2; exit }' "$secret_file" 2>/dev/null | tr -d '"' || true)
  public=$(awk '/OUTTURN_TOKEN_PUBLIC_KEY:/ { print $2; exit }' "$secret_file" 2>/dev/null | tr -d '"' || true)
  if [[ -n "$secret" && -n "$public" ]]; then
    if [[ "${#secret}" -ne 64 ]]; then
      bad "OUTTURN_TOKEN_SECRET is not 64 hex characters"
    else
      derived=$(outturn_public_key_of "$secret")
      if [[ "$derived" != "$public" ]]; then
        bad "OUTTURN_TOKEN_PUBLIC_KEY is not the public half of OUTTURN_TOKEN_SECRET"
      else
        note "OK       signing keypair agrees"
      fi
    fi
  else
    bad "could not read the token keypair out of $secret_file"
  fi

  # Which cluster is about to receive this. Not a pass or a failure -- only
  # the person running it knows whether that is the right one.
  local context
  context=$(kubectl config current-context 2>/dev/null || echo "none")
  note "context  $context"
}

if [[ "$check_only" == true ]]; then
  check
  if [[ "$fail" != 0 ]]; then
    echo >&2
    echo "not safe to deploy" >&2
    exit 1
  fi
  echo >&2
  echo "checks passed" >&2
  exit 0
fi

# ---------------------------------------------------------------------------
# Generate. Refuses to overwrite: replacing a live signing key signs everybody
# out, and doing it because a script was run twice is not a decision anybody
# made.
# ---------------------------------------------------------------------------
if [[ -f "$secret_file" ]]; then
  echo "$secret_file already exists." >&2
  echo "Delete it to generate new keys -- which signs out every existing session -- or" >&2
  echo "run with --check-only to check what is there." >&2
  exit 1
fi

echo "Generating a production secret for $dir." >&2
echo "Nothing is applied to any cluster; the manifests are written for you to review." >&2
echo >&2

# Reads from the terminal when there is one, and from stdin when there is
# not, so the questions can be answered by a here-document in a test or a
# rehearsal without the script needing a mode for it.
if { exec 3</dev/tty; } 2>/dev/null; then
  : # a terminal to ask at
else
  exec 3<&0
fi

ask() {
  local prompt="$1" default="${2:-}" answer=""
  if [[ -n "$default" ]]; then
    read -rp "$prompt [$default]: " answer <&3 || true
    printf '%s' "${answer:-$default}"
  else
    while :; do
      if ! read -rp "$prompt: " answer <&3; then
        # End of input with nothing for a question that has no default: a
        # deployment configured by whatever bash left in the variable is
        # worse than one that stopped.
        echo >&2
        echo "no answer for \"$prompt\", and it has no default" >&2
        exit 2
      fi
      [[ -n "$answer" ]] && break
      echo "  required" >&2
    done
    printf '%s' "$answer"
  fi
}

database_url=$(ask "DATABASE_URL (postgres://user:pass@host:5432/outturn)")
s3_endpoint=$(ask "OUTTURN_S3_ENDPOINT")
s3_bucket=$(ask "OUTTURN_S3_BUCKET" "outturn")
s3_access=$(ask "OUTTURN_S3_ACCESS_KEY")
s3_secret=$(ask "OUTTURN_S3_SECRET_KEY")
default_model=$(ask "OUTTURN_DEFAULT_MODEL")

secret=$(outturn_token_secret)
public=$(outturn_public_key_of "$secret")
runtime=$(outturn_runtime_key)

mkdir -p "$dir"
umask 077
cat >"$secret_file" <<EOF
# Generated by scripts/deploy-prod.sh. Not for committing.
#
# The token keypair belongs together: OUTTURN_TOKEN_PUBLIC_KEY is the public
# half of OUTTURN_TOKEN_SECRET, and replacing either alone leaves the API
# minting tokens the gateway and runtime refuse. Replacing both signs out
# every existing session, which is the intended effect of a rotation and a
# surprise otherwise.
#
# There is no dev seed here on purpose: the first account is made by hand, so
# that no deployment ever has one nobody created.
apiVersion: v1
kind: Secret
metadata:
  name: outturn-secrets
type: Opaque
stringData:
  OUTTURN_TOKEN_SECRET: "$secret"
  OUTTURN_TOKEN_PUBLIC_KEY: "$public"
  OUTTURN_RUNTIME_KEY: "$runtime"
  DATABASE_URL: "$database_url"
  OUTTURN_S3_ENDPOINT: "$s3_endpoint"
  OUTTURN_S3_BUCKET: "$s3_bucket"
  OUTTURN_S3_ACCESS_KEY: "$s3_access"
  OUTTURN_S3_SECRET_KEY: "$s3_secret"
  OUTTURN_DEFAULT_MODEL: "$default_model"
EOF

echo >&2
echo "wrote $secret_file" >&2
echo >&2
check

echo >&2
if [[ "$fail" != 0 ]]; then
  echo "not safe to deploy" >&2
  exit 1
fi

cat >&2 <<EOF
Next:
  1. Review $secret_file, and keep it out of version control.
  2. Write $dir/kustomization.yaml pointing the deployments at the
     outturn-secrets Secret -- k8s/overlays/local shows the shape, with
     secretKeyRef in place of each value.
  3. kubectl apply -k $dir
EOF

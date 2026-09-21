# Key generation, shared by the dev and production scripts.
#
# Sourced, not run. Everything here writes hex to stdout and nothing else, so
# a caller can capture it; anything to say to a person goes to stderr.

# The Ed25519 seed the API signs with, as 64 hex characters -- the length
# `hex_to_bytes` in src/auth/token.rs insists on.
outturn_token_secret() {
  openssl rand -hex 32
}

# The public half of a seed, which is what the gateway and runtime verify
# with. Derived rather than generated: a public key that does not belong to
# the secret would leave every tier rejecting every token the API minted,
# which is the failure src/auth/token.rs:149 describes having already been
# through once.
#
# Done by handing OpenSSL a DER-wrapped private key, because there is no
# openssl subcommand that takes a bare Ed25519 seed. The prefix is the fixed
# PKCS#8 header for one: SEQUENCE, version 0, the Ed25519 OID (1.3.101.112),
# and an OCTET STRING of 32 bytes. Only the last 32 bytes of the DER public
# key are the key itself. Checked against ed25519-dalek's own derivation --
# the two agree, which is what matters, since dalek is what reads these.
#
# macOS ships LibreSSL as `openssl`, and LibreSSL cannot load an Ed25519 key
# at all -- it answers "unable to load key". With the error discarded that
# produced an empty public key, written into the secret without complaint, and
# a gateway that panicked at startup with "no public keys to verify with". So
# the openssl that can do it is found rather than assumed, and a machine with
# none is told so instead of being handed a key that is not one.
outturn_openssl() {
  local candidate
  for candidate in \
    "${OUTTURN_OPENSSL:-}" \
    /opt/homebrew/opt/openssl@3/bin/openssl \
    /opt/homebrew/opt/openssl/bin/openssl \
    /usr/local/opt/openssl@3/bin/openssl \
    openssl
  do
    [ -n "$candidate" ] || continue
    command -v "$candidate" >/dev/null 2>&1 || continue
    # Asked rather than inferred from a version string: what matters is
    # whether this build does Ed25519, and the cheapest way to know is to
    # make it do one.
    if "$candidate" genpkey -algorithm ed25519 -outform DER >/dev/null 2>&1; then
      printf '%s' "$candidate"
      return 0
    fi
  done
  echo "no openssl here can do Ed25519 (macOS ships LibreSSL, which cannot);" \
       "install one with: brew install openssl@3" >&2
  return 1
}

outturn_public_key_of() {
  local seed="$1" der ssl pub
  ssl=$(outturn_openssl) || return 1
  der=$(mktemp)
  printf '302e020100300506032b657004220420%s' "$seed" | xxd -r -p >"$der"
  pub=$("$ssl" pkey -inform DER -in "$der" -pubout -outform DER 2>/dev/null |
    tail -c 32 | xxd -p -c 64)
  rm -f "$der"
  # A short answer is a failed derivation, and writing it would hand the
  # gateway a key that cannot verify anything the API signs.
  if [ "${#pub}" -ne 64 ]; then
    echo "could not derive the public key from the seed" >&2
    return 1
  fi
  printf '%s' "$pub"
}

# What the runtime presents to take work. Not a signing key and never
# verified as one: it says "the runtime tier" and nothing more, so any
# unguessable string of at least RUNTIME_KEY_MIN_BYTES (32) will do.
outturn_runtime_key() {
  printf 'outturn-runtime-%s\n' "$(openssl rand -hex 24)"
}

# A password for a seeded local admin, in a shape somebody can retype from a
# terminal without checking each character.
outturn_dev_password() {
  openssl rand -hex 12
}

# Refuses a value that is one of the dev ones this repository used to commit.
# They are in the git history and will stay reachable there, so a deployment
# that picked one up -- from an old overlay, a copied command, a stale
# Secret -- is holding a published key.
outturn_is_published_key() {
  case "$1" in
    993c3d8e41668abaa0151de741215ef5bf5022b62bdb8468122df597c70d5887) return 0 ;;
    433388b76ceaa6de2db078d26935cbcff46207149af743d530a04a023ad13c54) return 0 ;;
    outturn-dev-runtime-key-0123456789abcdef0123456789abcdef) return 0 ;;
    outturn-dev | outturn-dev-password) return 0 ;;
    *) return 1 ;;
  esac
}

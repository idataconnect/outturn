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
outturn_public_key_of() {
  local seed="$1" der
  der=$(mktemp)
  printf '302e020100300506032b657004220420%s' "$seed" | xxd -r -p >"$der"
  openssl pkey -inform DER -in "$der" -pubout -outform DER 2>/dev/null |
    tail -c 32 | xxd -p -c 64
  rm -f "$der"
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

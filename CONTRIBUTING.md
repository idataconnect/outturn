# Contributing

Thanks for looking. This is an early project and outside contributions are
welcome.

## Before a large change

Open an issue first. Not for process -- for your own sake, so you do not write
something substantial against a design that was about to move.
[docs/roadmap.md](docs/roadmap.md) says where it is going, and
[AGENTS.md](AGENTS.md) is the working document for how it fits together.

Small fixes need no ceremony. Send the pull request.

## Getting it running

[README.md](README.md) covers the Mac and Linux quickstarts. You need a local
cluster, ollama, and a moderately large machine -- the default local model
takes 18-23GB loaded.

## Tests

The suites are split by what they cost and what they need, not by how they are
written:

```
cargo test                                # needs nothing external
cargo test --features slow-tests          # a WASM sandbox per case; saturates the machine
cargo test --features integration-tests   # Postgres and a gateway running
cargo test --features live-providers      # calls a real provider, on your key, for money
```

`cargo test` is what CI runs and what a bare clone can run. Keep it that way:
a test that quietly grows a dependency on a running service belongs behind a
feature flag.

A missing environment variable fails rather than skips. A skipped test
reporting success is how a misconfigured run goes green having tested nothing.

For the UI, in `ui/`: `npm test`, `npm run lint`, `npm run build` (which
typechecks). CI runs all three.

Before pushing:

```
cargo fmt --all
cargo clippy --all-targets -- --deny warnings
```

## What we look for

**Tests that describe a scenario.** The convention here is that a test name
says what should be true, and the body arranges the situation that would break
it. Several subsystems were rebuilt after a real incident, and the tests are
the record of what went wrong -- a test that only exercises a line is less
useful than one that pins a behaviour somebody was surprised by.

**Comments that say why.** The code is commented more heavily than most, and
deliberately: almost always about the reason, the alternative that was
rejected, or the failure that motivated the shape. A comment restating the
line below it is noise. A comment saying what broke last time is the most
valuable thing in the file. See `scripts/lib/keys.sh` or `build.rs` for the
register.

**Commit messages in the imperative, describing the behaviour.** No
conventional-commits prefixes. `git log` shows the style -- "Let a trigger
declare its scheme, because not every sender can sign", not
"fix(triggers): scheme".
Say what changes and, when it is not obvious, why.

**Respect for the tier boundaries.** api holds the signing key and the
database, gateway holds the credentials, runtime holds nothing. A change that
moves a secret across one of those lines needs to argue for itself.
[SECURITY.md](SECURITY.md) states the boundary; AGENTS.md has the invariants
in detail.

## Security

Do not open an issue for a vulnerability. [SECURITY.md](SECURITY.md) has the
private reporting path.

## Licence

Contributions are under [Apache-2.0](LICENSE), the project's licence. There is
no CLA.

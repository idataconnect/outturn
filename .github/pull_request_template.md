## What and why

<!-- What this changes, and the reason for it. Link the issue it closes, if any. -->

## How it was tested

<!-- Say which ran, and anything tried by hand. -->

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --all-targets -- --deny warnings`
- [ ] `cargo test`
- [ ] `cargo test --features integration-tests`, if the change could break something only those suites catch (see CONTRIBUTING.md)
- [ ] In `ui/`: `npm run lint`, `npm test` and `npm run build`, if the UI changed
- [ ] `scripts/check-k8s-parity.py`, if `k8s/base` or `k8s/deploy` changed

## Docs

- [ ] Updated in `docs/`, or nothing a reader would notice changed

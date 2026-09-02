# outturn

*You define the process, outturn delivers the result.*

A multitenant agent platform.

## Tests

`cargo test` runs only what needs nothing external. That includes the agent
component tests, which run against a scripted in-process gateway rather than a
live model -- fast, deterministic, and able to arrange failures a real provider
makes awkward, such as a 503 or a stream truncated mid-generation.

The suites that need running services are gated behind a feature flag, so a
bare checkout builds and runs the fast tests without silently skipping the
rest. They are separated by what they require rather than by how they are
written:

```
# Postgres and a gateway, with skaffold dev providing the port-forwards.
# Note the *_test database: these create and drop schemas, and refuse to run
# against a database whose name does not contain "test".
TEST_DATABASE_URL=postgres://outturn:outturn-dev@localhost:15432/outturn_test \
GATEWAY_URL=http://localhost:18081 \
  cargo test --features integration-tests
```

Each database test runs in its own Postgres schema, created on setup and
dropped on success, so they are isolated from each other and run in parallel.
A failing test leaves its schema behind to inspect.

A missing environment variable fails rather than skipping. A skipped test that
reports success would let a misconfigured CI run green having tested nothing.

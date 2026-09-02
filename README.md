# outturn

*You define the process, outturn delivers the result.*

A multitenant agent platform.

## Tests

`cargo test` runs only what needs nothing external.

The suites that need running services are gated behind a feature flag, so a
bare checkout builds and runs the fast tests without silently skipping the
rest. They are separated by what they require rather than by how they are
written:

```
# Postgres and a gateway, with skaffold dev providing the port-forwards.
# Note the *_test database: these truncate every table, and refuse to run
# against a database whose name does not contain "test".
TEST_DATABASE_URL=postgres://outturn:outturn-dev@localhost:15432/outturn_test \
GATEWAY_URL=http://localhost:18081 \
  cargo test --features integration-tests -- --test-threads=1
```

`--test-threads=1` is required: the integration tests share one database and
reset it between tests.

A missing environment variable fails rather than skipping. A skipped test that
reports success would let a misconfigured CI run green having tested nothing.

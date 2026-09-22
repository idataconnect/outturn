# Security

## Reporting a vulnerability

Please report privately, not as a public issue.

Use GitHub's [private vulnerability
reporting](https://github.com/idataconnect/outturn/security/advisories/new),
which goes to the maintainers and nobody else.

Tell us what you can reach and how, and include enough to reproduce it -- the
tier you started from matters as much as the bug. We will acknowledge within
a week, and keep you posted while we work on it. If we disagree that something
is a vulnerability we will say so and explain why, rather than let the report
go quiet.

This is a young project with no paid bounty. What we can offer is a fix, an
advisory that credits you however you would like to be credited, and an honest
timeline.

## Scope

What this project is trying to prevent is the interesting part, so the boundary
is worth stating plainly. There are three tiers, and they hold different
things:

- **api** holds the token signing key and the database.
- **gateway** holds the outbound credentials. It verifies tokens with a public
  key and never sees the private one.
- **runtime** runs workspace agent code in a WebAssembly sandbox. It holds
  nothing. Its outbound traffic is deny-by-default and goes through the
  gateway.

So these are in scope, and we want to hear about them:

- Anything that lets workspace agent code escape the sandbox, or reach a host,
  credential, or service its rules do not permit.
- Anything that lets one workspace read, alter, or affect another's data,
  sessions, or traffic.
- Token forgery, replay across workspaces or turns, or privilege escalation
  between the three tiers.
- Defeating the egress rules: SSRF, DNS rebinding, or getting the gateway to
  attach a credential to a request that should not carry it.
- Anything that makes the api or gateway hand out a secret it holds.

Known and deliberate, so not vulnerabilities:

- **The dev keys in git history are published, on purpose.** Early commits
  contained local-cluster keys; they were removed in `cece1c9` and remain
  reachable in history, as anything committed does. They were never real
  credentials. `outturn_is_published_key` in `scripts/lib/keys.sh` lists them,
  and `scripts/deploy-prod.sh` refuses to deploy a manifest containing one.
  Secret scanners will flag them. If you find a deployment *using* one, that
  is a finding worth reporting -- the values themselves are not.
- The `k8s/base` and `k8s/overlays/local` manifests are development
  configuration, with known passwords and permissive settings. They are not
  meant for a real cluster and the production script will not emit them.
- A compromised runtime can observe other tenants sharing that same host
  process. Limiting the blast radius to exactly that is the current design;
  narrowing it further is ongoing work, and a way to reach *beyond* the shared
  host is in scope.
- Anything needing an attacker who already holds the api's signing key or
  direct database access. That is the tier the whole model trusts.

## Supported versions

Pre-1.0 and moving quickly. Fixes land on `main`, and there are no
maintained release branches yet. If you are running this anywhere that
matters, track `main`.

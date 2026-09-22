# hollowbrook

A guesthouse that never was, running beside outturn.

The base deploys it at zero replicas; this scales it to one. `internal-host`
names what the gateway has to be allowed to reach, which
`scripts/dev-mac.sh` collects into `OUTTURN_INTERNAL_HOSTS` along with every
other component's.

    scripts/dev-mac.sh --with hollowbrook

## What is still manual

**The workspace's egress rule.** The operator's list says the gateway may
reach `outturn-hollowbrook:8084`; a workspace still has to allow that host in
its own rules before an agent can ask it to.

**A skill describing the API.** Hollowbrook serves `/openapi.json`, so the
OpenAPI wizard is the eventual answer. Until then a skill is written by hand.

**The look.** `VITE_THEME=hollowbrook` and the `VITE_BRAND_*` variables reskin
the UI, but the UI is not in the cluster -- it is `npm run dev` on the host,
and Vite reads those at build time. A component cannot set them.

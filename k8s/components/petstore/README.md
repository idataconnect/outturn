# petstore

Swagger's sample Pet Store, for building an OpenAPI consumer against a real
specification rather than one written to suit the thing consuming it.

`internal-host` is the name and port the gateway has to be allowed to reach.
`scripts/lib/dev.sh` collects that file from every component asked for and
writes them into `OUTTURN_INTERNAL_HOSTS` as one list -- which is why the
component does not set the variable itself. See the comment in
`kustomization.yaml`, and "An operator allowlist, by name" in docs/egress.md.

A workspace still has to allow `petstore` in its own egress rules. The
operator's list says the gateway *may* go there, not that anyone may ask it
to.

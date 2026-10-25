# Deploying outturn

Running a release rather than a checkout: published images, a Kubernetes base
that runs them, and an infrastructure repository of your own that pins which
commit. [local-development.md](local-development.md) is the other way to run
it, and shares almost nothing with this.

## What there is

**Images.** CI publishes four to this repository's container registry once it
has passed on a commit (`.github/workflows/images.yml`):

| Image | Is |
|---|---|
| `ghcr.io/<owner>/outturn-api` | The API, the queue and its worker. Migrates the schema on start |
| `ghcr.io/<owner>/outturn-gateway` | Model providers and every outbound request. Holds the credentials |
| `ghcr.io/<owner>/outturn-runtime` | The WASM sandbox agents run in |
| `ghcr.io/<owner>/outturn-ui` | The built UI on a static file server, carrying `/v1` through to the API |

Each push to main is tagged with its full commit SHA; a release tag `v1.2.0`
publishes `1.2.0` as well. There is no `latest`. A weekly job
(`image-cleanup.yml`) deletes untagged versions and every commit build but the
newest 30 of each image; anything carrying a release version is kept for good.
Roll back further than 30 commits by deploying a release.

The packages are private when first published. Make them public in the
repository's package settings, or give the cluster a pull secret.

**`k8s/deploy`.** The three tiers and the UI, their Services, disruption
budgets and network policies -- and no Postgres, no object store and no mocks.
Those are yours, named in a Secret. The development cluster's `k8s/base` is
separate and keeps its own.

**PostgreSQL 18, with pgvector.** The migrations call `uuidv7()`, which is new
in 18, and create the `vector` extension, so an older server or one without
the extension fails the API's first start rather than some later query. 18 is
what the development cluster runs and the only major tested; a later one is
expected to work.

## Secrets to generate

Five keys, generated once per deployment and never shared with another:

| Key | Tier | What it is |
|---|---|---|
| `OUTTURN_TOKEN_SECRET` | api | Ed25519 seed, 64 hex. Signs every token |
| `OUTTURN_TOKEN_PUBLIC_KEY` | api, gateway | Its public half. Must be derived from the seed, not generated beside it |
| `OUTTURN_RUNTIME_KEY` | api, runtime | What a runtime presents to take work; 32+ bytes of anything unguessable |
| `OUTTURN_SEAL_KEY` | gateway | X25519 private key, 64 hex. Opens sealed credentials |
| `OUTTURN_SEAL_PUBLIC_KEY` | api | Its public half, handed to the browser to seal to |

`scripts/deploy-prod.sh` generates all five, asks for the database and object
store, and writes a `secret.yaml` with exactly the keys `k8s/deploy` reads. It
derives the public halves with OpenSSL (`scripts/lib/keys.sh` shows how; there
is no OpenSSL command that takes a bare seed, so the script wraps it in a
fixed PKCS#8 header) and checks the token pair agrees. Use it rather than
deriving by hand: a public key that does not belong to its seed starts three
healthy-looking tiers that refuse each other's tokens.

Keep the generated file out of version control. Put it in the cluster by
whatever your infrastructure uses for secrets -- a sealed or external secret
in the infrastructure repository, or `kubectl apply` once by hand.

Losing `OUTTURN_SEAL_KEY` makes every sealed credential unopenable, backups
included; keep a copy where you keep the database's backups' keys. Rotating
either keypair is in [sealed-credentials.md](sealed-credentials.md) and
[invariants.md](invariants.md).

## The Secret and the ConfigMap

Both in the namespace outturn runs in.

**Secret `outturn-secrets`** -- the five keys above, and:

| Key | Read by |
|---|---|
| `DATABASE_URL` | api, gateway. `postgres://user:pass@host:5432/outturn` |
| `OUTTURN_S3_ENDPOINT` | api, runtime. An S3-compatible endpoint URL |
| `OUTTURN_S3_BUCKET` | api, runtime. Created beforehand |
| `OUTTURN_S3_ACCESS_KEY` | api, runtime |
| `OUTTURN_S3_SECRET_KEY` | api, runtime |

Each tier is handed only its own keys, by name: the signing secret reaches
the API alone and the seal key the gateway alone.

**ConfigMap `outturn-config`** -- only the first is required:

| Key | Read by | Is |
|---|---|---|
| `OUTTURN_CORS_ORIGINS` | api | The public origin, `https://agents.example.com`. The API refuses a credentialed request from any other, so without it nobody can sign in |
| `OUTTURN_BRAND_NAME` | api, ui | What the product calls itself, in the UI and in every system prompt's preamble |
| `OUTTURN_BRAND_LOGO` | ui | A same-origin path to the mark, such as `/brand/logo.svg` |
| `OUTTURN_BRAND_TAGLINE` | ui | A line under the name on the login page |
| `OUTTURN_THEME` | ui | A shipped theme by name (`hollowbrook`), or `/brand/<file>.css` |
| `OUTTURN_INTERNAL_HOSTS` | api, gateway | Private hosts an egress rule may still reach; see [egress.md](egress.md) |
| `OUTTURN_CREDENTIAL_BINDINGS` | api, gateway | See [credential-bindings.md](credential-bindings.md) |
| `OUTTURN_TIKA_URL` | api, runtime | Document extraction; see [document-extraction.md](document-extraction.md) |
| `OUTTURN_PDF_RENDERER_URL` | runtime | See [pdf-rendering.md](pdf-rendering.md) |
| `OUTTURN_CATALOG_FETCH` | api | `off` to stop the weekly catalog fetch |

The branding is read when the UI container starts, not when it was built, so
the published image serves every deployment; [themes/README.md](https://github.com/idataconnect/outturn/blob/main/ui/src/themes/README.md)
says how to write a theme. A logo or theme of your own is a file mounted into
the UI container at `/srv/brand/` -- from a ConfigMap, say -- and named by its
path under `/brand/`.

The UI container reads one more variable, set in `k8s/deploy` itself rather
than the ConfigMap: `OUTTURN_API_UPSTREAM`, the `host:port` it carries `/v1`
to, `outturn-api:8080` by default. Change it only if the API's Service is
renamed or lives in another namespace.

Models are not configured here. Sign in and go to Platform → Models, which
offers a recipe to a deployment with none; provider keys typed there are
sealed in the browser to `OUTTURN_SEAL_PUBLIC_KEY` and never pass through the
environment. Until a model is configured every turn is refused.

## An infrastructure repository

A repository of your own holding one kustomization, which pins a commit of
this one:

```
infra/
  outturn/
    kustomization.yaml
    secret.yaml          # however your secrets are kept; not in the clear
```

```yaml
apiVersion: kustomize.config.k8s.io/v1beta1
kind: Kustomization
namespace: outturn

resources:
  # The commit's manifests. Pin the same commit as the images below, so the
  # manifests and what they run agree.
  - https://github.com/idataconnect/outturn//k8s/deploy?ref=<full SHA>
  - secret.yaml

images:
  - name: ghcr.io/idataconnect/outturn-api
    newTag: <full SHA>
  - name: ghcr.io/idataconnect/outturn-gateway
    newTag: <full SHA>
  - name: ghcr.io/idataconnect/outturn-runtime
    newTag: <full SHA>
  - name: ghcr.io/idataconnect/outturn-ui
    newTag: <full SHA>

configMapGenerator:
  - name: outturn-config
    literals:
      - OUTTURN_CORS_ORIGINS=https://agents.example.com
      - OUTTURN_BRAND_NAME=Example Agents

patches:
  # Where the object store is. Shipped naming a range that reaches nothing,
  # because the runtime is one of the pods reaching it -- see below.
  - target:
      kind: NetworkPolicy
      name: outturn-object-storage
    patch: |
      - op: replace
        path: /spec/egress/0/to/0/ipBlock/cidr
        value: 203.0.113.10/32
```

A release is the same with its version in place of the SHA, in `ref=v1.2.0`
and `newTag: 1.2.0`. An upgrade is a commit to this file; a rollback is
reverting it -- with the caveat about the database below.

The generated ConfigMap gets a hash suffix that changes with its contents, so
changing a value rolls the pods that read it. The Secret, applied by name,
does not: restart the tiers after changing it.

## Ingress, TLS and the network

Point the ingress at the `outturn-ui` Service on port 8080, and nothing at the
API: the UI carries `/v1` to it on the same origin, which the refresh cookie
needs -- it is scoped to `/v1/session/refresh` and a browser never sends it to
another path or host. Webhooks arrive the same way, at `/v1/hooks/`.

TLS is required, not advised. Session cookies are `Secure`, so a browser
reaching the deployment over plain HTTP anywhere but localhost signs in and is
immediately signed out. Terminate TLS at the ingress, and set
`OUTTURN_CORS_ORIGINS` to the `https://` origin people type.

A reply streams for as long as a model takes, and may sit silent for up to
five minutes; give the ingress a read timeout longer than that, and leave
response buffering off for `/v1`.

The network policies deny everything and open each path the tiers need. They
need a CNI that enforces them; on one that does not they apply and do
nothing. Two need your attention:

- **`outturn-ui`** admits the ingress controller by the name of the namespace
  it runs in. Change the name to yours.
- **`outturn-object-storage`** names the object store by address, and ships
  naming a documentation range. Replace it -- with a `podSelector` or
  `namespaceSelector` for a store inside the cluster. By address rather than
  "anywhere on 443" because the runtime is one of the two pods it covers, and
  that would hand a compromised runtime every HTTPS host on the internet: the
  one path every other policy here closes.

Neither Tika nor the PDF renderer is part of a release, and no policy here
opens a path to one. Setting `OUTTURN_TIKA_URL` or `OUTTURN_PDF_RENDERER_URL`
alone gets a refused connection -- documents that are never extracted, a PDF
tool that fails -- rather than an error saying why.
`k8s/deploy/examples/document-services-networkpolicy.yaml` opens both: the API
to Tika, and the runtime to the renderer and nothing further. The runtime
reads `OUTTURN_TIKA_URL` only to know extraction is on, and is not let
through to Tika. A sketch of one half, for a renderer in the same namespace:

```yaml
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: outturn-runtime-to-pdf-renderer
spec:
  podSelector:
    matchLabels:
      app: outturn-runtime
  policyTypes: [Egress]
  egress:
    - to:
        - podSelector:
            matchLabels:
              app: outturn-pdf-renderer
      ports:
        - protocol: TCP
          port: 8090
```

The renderer's own pod is under the default deny too, so the example also
admits the runtime to it.

The database is reached by port alone (5432, anywhere), since a managed
database's address rarely stays put and neither the API nor the gateway runs
workspace code. Narrow it if yours has a stable range.

## Upgrades

Take a backup first. Then, in this order:

1. **The gateway.** It reads what the API commits -- the routes a model
   chain is made of, sealed credentials, the egress rules a turn is held to
   -- so a gateway that understands the new shape has to be running before an
   API starts writing it.
2. **The API.** Migrations run as it starts, and only go forward: there is no
   down migration, and an older API against a newer schema is not supported.
   With more than one replica the first new pod migrates and the rest wait on
   the migration lock.
3. **The runtimes and the UI**, in either order. The runtime relays what the
   gateway and API send it without reading it, so it may lag a release.

With an infrastructure repository that is either three commits, or one commit
and `kubectl rollout status` on the gateway before letting the rest proceed --
whatever applies the kustomization applies all four Deployments at once
otherwise. A release's notes say when the order matters to it; when it does not
say, follow it anyway.

## Backups

The database is the deployment: workspaces, transcripts, sealed credentials,
usage. `k8s/deploy/examples/backup-cronjob.yaml` is a nightly `pg_dump` in the
custom format to a volume, keeping fourteen, with its network policy. Its
client is Postgres 18, and `pg_dump` refuses a server newer than itself, so
raise the image when the server moves. Add it
to the infrastructure kustomization's resources and change where the dumps go:
a volume beside the database survives a bad migration but not the loss of the
cluster. A managed database's own snapshots are as good, and better if they
are point-in-time.

The object store holds uploads and a session's files; back it up by the
store's own means. A database restored without it has transcripts whose
attachments are missing, which is survivable. Sealed credentials in a dump are
useless without the seal key, which is the point -- keep the key, separately.

## Rollback

Migrations only go forward, so rolling back the images alone is not a
rollback: the older API finds migrations it does not know applied, and
refuses to start. A rollback is:

1. Scale the API, gateway and runtimes to zero, so nothing writes.
2. Restore the backup taken before the upgrade:
   `pg_restore --clean --if-exists --no-owner --dbname "$DATABASE_URL" outturn-<when>.dump`.
3. Point the infrastructure repository back at the previous SHA, and apply.

Anything written between the backup and the rollback is lost, which is the
reason to take the backup immediately before upgrading.

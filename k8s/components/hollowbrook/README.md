# hollowbrook

A guesthouse that never was, running beside outturn, with a skill that lets an
agent use its API.

    scripts/dev-mac.sh --with hollowbrook

Three things happen. The deployment scales from the zero replicas the base
leaves it at. `internal-host` is collected into `OUTTURN_INTERNAL_HOSTS`, which
is what lets the gateway reach a private address at all. And a Job installs the
skill.

## Why the Job

This is the shape a customer's own integration takes, which is the point of
doing it this way. `install.sh` signs in, creates a skill, approves its host
and uploads its files, using the endpoints outturn already publishes — nothing
privileged, and nothing patched into the platform.

The first version of this was a Rust module in `src/api/`. It worked, and it
was the wrong answer: a customer wiring up their own API cannot add a module to
outturn, and a fork to do it loses to every upgrade. The same argument
`ui/src/themes/README.md` makes about skinning holds here.

So the script is POSIX sh with curl and jq, and it is meant to be read and
adapted rather than reused as-is.

## The skill

`skill/index.md` is the manifest and becomes the skill body; the rest are one
file per operation, uploaded to `workspace/api/hollowbrook/`.

That split is [docs/openapi-wizard.md](../../../docs/openapi-wizard.md). A
skill body is composed into the system prompt on every round of every turn, so
an API written into one in full is paid for continuously whether or not it is
used. The manifest says what exists; the agent reads an operation's file with
`read_object` when it decides it needs that one.

Hollowbrook is too small to need this — 1.3KB of manifest against 5KB of
detail, where the whole API would fit in a body. It is done anyway because the
shape is what is being tried: whether a model that has read a manifest line
goes and reads the file, rather than guessing the call from the name, is what
the wizard rests on.

**The manifest names no URL.** A skill documenting its call the way an API
reference does reads correctly to a capable model and gets called as a tool
name by a weaker one — see
[docs/skill-evaluation.md](../../../docs/skill-evaluation.md). URLs appear in
the detail files, beside the method and an instruction to use `fetch_url`.

## What is still manual

**The look.** `VITE_THEME=hollowbrook` and the `VITE_BRAND_*` variables reskin
the UI, but the UI is not in the cluster — it is `npm run dev` on the host, and
Vite reads those at build time.

**Binding the skill to an agent.** Installed into the workspace, not bound:
which agents get it is a decision, and this component does not make it.

# hollowbrook

A guesthouse that never was, running beside outturn, with a skill that lets an
agent use its API.

    scripts/dev.sh --with hollowbrook        # scripts/dev-mac.sh on a Mac

Three things happen. The deployment scales from the zero replicas the base
leaves it at. `internal-host` is collected into `OUTTURN_INTERNAL_HOSTS`, which
is what lets the gateway reach a private address at all. And a Job installs the
skill, and an agent to try it with.

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
file per operation, published with it as files of the same version
([docs/skill-bundles.md](../../../docs/skill-bundles.md)). An agent reads them
as `skill/hollowbrook/<operation>.md`.

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

**Charging needs a person.** `charge_payment_account` declares
`approval: requires: charge` in its frontmatter, which is the one operation here
that money moves through. What that means, and why the rule lives in the
operation's own file rather than on the agent or in a setting, is
[docs/approvals.md](../../../docs/approvals.md).

**No card numbers, anywhere.** A *payment account number* -- `pa_4471` --
stands for a card the house holds, and no endpoint returns the card. That is
not squeamishness: a transcript is replayed to a model on every later turn, so
a card number that reached one would be in every subsequent prompt for the life
of the conversation. The fixture has a test asserting nothing it serves contains
a run of digits long enough to be a card.

**The manifest names no URL.** A skill documenting its call the way an API
reference does reads correctly to a capable model and gets called as a tool
name by a weaker one — see
[docs/skill-evaluation.md](../../../docs/skill-evaluation.md). URLs appear in
the detail files, beside the method and an instruction to use `fetch_url`.

## The agent

The Job also creates **Front desk** (`front-desk`), with
`system-prompt.txt` as its prompt and this skill bound, so the walkthrough
starts at a conversation rather than at a form. Only that agent: which of a
workspace's own agents get the skill is still the workspace's decision.

It is made once and then left alone, because a prompt edited in the UI must
survive the next redeploy. The one exception is an agent with no skills at
all, which is given this one -- creating and binding are two calls, and a Job
retried between them would otherwise leave an agent that cannot do what it
was made for.

## What is still manual

**The look.** The UI is not in the cluster -- it is `npm run dev` on the host,
and Vite reads these at build time, so this component cannot set them:

```
cd ui && VITE_THEME=hollowbrook VITE_BRAND_NAME='Hollowbrook House' \
  VITE_BRAND_LOGO=/hollowbrook-logo.svg npm run dev
```

`docs/take-it-for-a-spin.md` starts the UI that way, so somebody following the
walkthrough sees it without having to find this file.

# weather

A forecast skill, against [Open-Meteo](https://open-meteo.com) — a real public
API, free and without a key.

    scripts/dev-mac.sh --with weather

Or with the guesthouse, which is the interesting combination:

    scripts/dev-mac.sh --with hollowbrook,weather

## Why both

Hollowbrook is inside the cluster, so reaching it needs an operator to open
its host in `OUTTURN_INTERNAL_HOSTS` as well as a workspace rule. A public
host needs only the rule. Two components, two paths through the same gateway,
and the second is the one most integrations will actually take.

With both installed an agent has to *compose* them. The house's coordinates
live in the Hollowbrook skill, because that is a fact about the house rather
than about weather; the weather skill knows how to call an API and is told
nothing about where anywhere is. Answering "what will it be like when I
arrive?" means taking one from the first and handing it to the second.

## No credential

Open-Meteo wants no key, which keeps this about skills and rules. It does
therefore not demonstrate the gateway holding a credential on an agent's
behalf — that is the more important property and still wants a worked example.

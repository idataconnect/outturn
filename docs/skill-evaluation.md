# Telling somebody their skill is not working

Designed, unbuilt. Written down while the example that prompted it was in
front of us, because a design argued from a real transcript is worth more than
one argued from a guess.

## What happened

A skill documented its call the way its own API documentation does:

```
GET https://api.duckduckgo.com/?q=<query>&format=json
```

The agent has one tool for this, `fetch_url`, and the skill's prose is telling
the model to use it against that endpoint. A capable model makes that leap. The
one being run did not: it called a tool named
`GET https://api.duckduckgo.com/?q=gemma%204&format=json&no_html=1&no_redirect=1`,
with empty arguments, three times in one turn, burning about a thousand
completion tokens. The host answered each with `no such tool`, which is exactly
right and did not help. The model then gave up on the tool and answered from
memory -- asserting something about itself it had been told not to invent,
which is the failure the skill existed to prevent.

In a neighbouring session the same model called a tool named after the skill's
own heading, `DuckDuckGo Instant Answer Search`, with `{"query": ...}`. Same
mistake, different wrong name.

Nothing was broken. The sandbox held, the errors were reported, the turn ended.
The skill was simply written for a reader that infers, and was being read by one
that does not -- and nothing anywhere would have said so. It was found by a
person reading a transcript by hand.

## Two loops, and only one of them needs a model

**The mechanical loop is most of the value and costs nothing.** Every fact
needed is already recorded: a tool call carries `is_error` and its result, a
turn carries its completion tokens, and a reply records which skill version was
bound. So this is a query, not an inference:

- Tool calls that failed, by skill and by version. `no such tool` in particular
  is close to a diagnosis on its own -- the model tried to call something that
  does not exist, and a skill's prose is the usual reason it thought otherwise.
- Tokens spent on turns that produced no text, which is what a skill failing
  expensively looks like.
- Turns where the tool was abandoned and the model answered anyway. Harder to
  detect exactly; a reply with a failed call and no successful one is close.

Per skill version, so "did rewording it help" has an answer. `skill_versions`
already has the ordinals, and a reply already records what ran, so the
comparison is available rather than needing new plumbing.

That alone would have caught this, without a model and without anyone reading
anything.

**The judged loop is for turning a number into a recommendation.** "This skill
has a 40% tool-call error rate" is where the mechanical half stops. "The model
is calling `GET https://...` as a tool name because your skill documents the
call as an HTTP request line; name `fetch_url` and pass the URL as an argument"
is a judgement, and worth a model. Run on demand rather than continuously,
because it costs tokens per run and the mechanical signal is what decides when
it is worth spending them.

Both loops should meet a person rather than edit anything. A skill is prose
somebody owns, and an automatic rewrite is a change nobody reviewed to a thing
that governs what agents do.

## What a person adds

The mechanical signal says a skill is failing and the judge says why, but
neither knows what the person was trying to do. "I asked it to look something
up and it told me about itself instead" is information nothing else in the
system has, and it is the fastest route to the right diagnosis.

So the loop is: a signal (an error rate, or somebody saying "that went wrong"),
the transcript, optionally what the person says happened, and a recommendation
they accept, edit, or discard. What they accept becomes a new skill version,
which the mechanical half then measures.

## Who reviews

Whoever holds the authority to edit the skill. That is usually its owning
organization or the platform operator, and it may well be the person who hit
the problem -- what decides it is the authority, not which kind of user they
are. This is the same split as approving a skill's hosts, where opening one
needs the authority that writes an egress rule rather than the one that writes
skills: authoring is not consent, and neither is complaining.

## The part to get right first

**A judge reads untrusted text.** A transcript contains what a customer typed,
what an external host returned, and a skill body a workspace admin wrote. All
three reach the model doing the analysis, and that model's output is shown to
somebody with the authority to change a skill. That is a prompt-injection
target with a privileged reader, and it is the one part of this that is
dangerous rather than merely unbuilt.

At minimum: the analysis is data and never instructions, its output is a
recommendation a person applies rather than an edit, and the judge holds no
authority of its own -- it reads a transcript and writes prose, and cannot
publish a version, bind a skill, or open a host.

## Not decided

- Whether the judge reads one transcript or a sample across many, which changes
  what it can see and what it costs.
- Whether a smaller local model is enough. The task is "read this failure and
  say what about the prose misled it", which may not need a frontier model --
  and the failures being diagnosed here come from exactly the class of model
  that would be doing the diagnosing.
- Where the numbers live. The usage ledger already carries the workspace, the
  agent and the session per turn, so this may be a view over it rather than a
  table of its own.

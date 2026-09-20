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

Available, but weaker than it looks when the two versions ran against different
traffic -- last week's turns are not this week's, and a rate that improved may
only mean an easier week. Comparing versions properly wants the same cases put
to both, which is what the held set and McNemar's test below are for.

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

## Did the edit help, or trade one failure for another

A skill is edited to fix something. The pass rate goes from 71% to 74%, and
nobody can say whether that is an improvement or four cases fixed and three
broken. Prose has no type checker, so every edit is free to regress something
it was not about, and the comparison that would catch it is not the one a pass
rate makes.

**McNemar's test** is the comparison that does. Re-run the same fixed set of
cases before and after, and look only at the pairs that *disagree*: passing
before and failing now, against failing before and passing now. The cases that
did not change carry no information about the edit and are discarded. Five
fixed and four broken is a net gain of one that McNemar will call
indistinguishable from noise, which is exactly the verdict a person editing
prose needs to hear.

That requires a held set of cases rather than a stream of live traffic, and it
is the strongest argument for keeping one. Live turns say what is going wrong
now; a fixed set is the only thing that can say whether a change made it
better, because it is the only thing where before and after are the same
question.

**The set accumulates rather than being authored**, and the difference is the
whole feasibility of this. Asking a workspace to sit down and write test cases
for their skill is asking them to do a second job, and cases invented for the
purpose carry the author's idea of what the skill means -- which is exactly the
thing under test. Synthetic cases cannot show that prose is confusing, because
whoever wrote them already knew what it was supposed to say.

A flagged session is the alternative and it is free. Real inputs, a real
failure, and a person's judgement that it went wrong -- which is the only
labelling that is ever going to happen here. Enough of them and there is a
corpus nobody wrote, made of things that actually broke.

So the order is flagging first, cases accumulate, and this test becomes
available when there are enough of them. Not a corpus to build before
starting.

**Wilson score intervals** for the rates themselves. A skill with four
failures in twenty turns has a failure rate somewhere between about 7% and 40%,
and the naive interval around 20% is not only wrong but can extend past zero
when the count is small -- which is the common case, since most skills are not
run thousands of times. Wilson behaves near the ends and at small n, and it is
a few lines of arithmetic rather than a dependency.

Both are queries. Neither needs a model, which matters because the
model-reading half is the dangerous half and the expensive half, and this is
the question people most want answered.

They are not equally ready, though. Wilson costs a few lines and needs nothing
that does not exist, so it belongs wherever a rate is shown, from the first
one. McNemar needs the accumulated set above, which needs flagging, which does
not exist -- so it is the right thing to reach for later and the wrong thing to
build toward now. The test is fifteen lines; everything expensive about it is
the corpus.

A skill generated from an API specification
([openapi-wizard.md](openapi-wizard.md)) deliberately carries no worked
examples, because an example the generator invented is a guess about what a
model will do, sitting in a file the agent trusts.

Evaluation is where a real example comes from. A turn that actually called an
operation is evidence rather than conjecture, and it attaches to the
operation's detail file -- which the agent already fetches lazily, so nothing
new carries it and nothing grows in the prompt.

That makes this the second half of the wizard rather than a separate feature,
and it is the clearest use for the successful half of a transcript. Most of
this document is about diagnosing failures; the successes are worth keeping
too, and this is what they are for.

## What a success signal can and cannot say

The loop this describes improves a skill from what happened when agents used
it. That only works if the signal means what it is taken to mean, and the
tempting signals mostly do not.

Three kinds, and only the first is safe to act on unsupervised:

**The world answered.** A probe that found the record, a uniqueness constraint
that refused a duplicate, an error code the recipient chose to send. Nothing is
inferred: something outside the platform was asked and replied.

**The mechanism worked.** The call was well-formed, the arguments parsed, the
reply was not truncated, the status was 2xx. This says the *form* was right and
nothing whatever about whether it was the right call to make. An agent that
fetches the wrong customer's invoices gets a clean 200 every time.

**A person accepted the outcome.** The only signal about correctness, and it is
sparse and expensive.

The hazard is the second kind wearing the first's clothes. A worked example
derived from a 2xx is honest about argument shape and dishonest about intent,
and promoting one teaches every later agent that this is what to ask for. The
artefact is prose a model reads, so there is no type error and no failing test
-- just a skill that quietly recommends a mistake.

So: form may be learned from the mechanism, intent may not. An example may show
how an operation is called. It must not imply that calling it that way was the
right thing to do.

## A flag invalidates what was learned from it

Somebody marking a session as wrong is a person's judgement arriving after the
automated signal, about the same evidence. It is the correction channel the
loop otherwise has no way to hear, and it has to do more than stop future
harm.

**It retracts.** By the time a session is flagged, anything derived from it may
already be attached and already shaping turns. So every automatically acquired
artefact records where it came from -- this example, from that turn -- and a
flag withdraws what its session produced. Without that link a flag can only
prevent, and what is already live stays live.

**It teaches the loop where it cannot trust itself.** One flagged session says
one example was wrong. Several against the same operation say the signal is
unreliable *there*: that operation returns success for calls that are wrong, so
well-formedness means nothing for it and the loop should stop acting on it
unsupervised. That is worth more than any individual retraction.

**Its absence proves nothing.** Flags are sparse and biased toward what
somebody noticed. Nobody flags a session that quietly did the wrong thing and
was never read. So a flag is strong negative evidence, and the lack of one is
not evidence of anything.

## Silence is the goal, but not secrecy

The loop earns its keep by not demanding attention. A platform that asks its
users to review every proposed improvement has given them a second job, and
they will stop reading it -- which is worse than not asking, because the
ignored prompt looks like consent.

So an improvement that cannot be wrong should apply itself and say nothing: a
probe resolving an `attempted` record, an example recording the argument shape
of a call that demonstrably parsed. One that could be wrong waits for a person,
and the bar for interrupting is that nobody else could have made the call.

What silence must not mean is unrecorded. A platform quietly changing its own
behaviour with no trail is the failure [inhibitors.md](inhibitors.md) already
names for summaries, where a misstatement becomes the record nobody can see
being made. Every automatic change is visible after the fact, attributable to
the turn it came from, and reversible by a flag.

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

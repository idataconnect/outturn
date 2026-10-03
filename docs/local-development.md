# This machine's model

A local cluster needs three facts that belong to the machine rather than to
the code: which model answers, how large a context window it runs at, and
how much of a conversation outturn sends it. The first run of `scripts/dev.sh`
or `scripts/dev-mac.sh` asks for them and keeps the answers; later runs use
them without asking.

```bash
scripts/dev-setup.sh --show          # what this machine is using, and where it is written
scripts/dev-setup.sh --reconfigure   # ask again, with the current answers as suggestions
scripts/dev.sh --reconfigure         # the same, then start the loop
```

Or edit `k8s/overlays/local/dev-machine.env` by hand; it is plain
`KEY=value`, gitignored, and read on every run.

## The answers, and what each one drives

| Answer | Drives |
|---|---|
| `OUTTURN_DEV_SERVER` | `ollama`, `llama.cpp`, or `other` -- which server the script prepares |
| `OUTTURN_DEV_MODEL` | What is pulled, and what the served model is made from |
| `OUTTURN_DEV_CONTEXT_WINDOW` | The window that model is served at |
| `OUTTURN_DEV_CONTEXT_BUDGET` | The operator's `context_budget`, in bytes |
| `OUTTURN_DEV_OLLAMA_URL` | Where the script reaches ollama from this machine, when that is the server |
| `OUTTURN_DEV_LLAMA_URL` | Where the script reaches llama-server from this machine, when that is the server |

A file from before there was a choice of server has no `OUTTURN_DEV_SERVER`,
and is read as it always meant: ollama where it names an ollama address, some
other server where it leaves it empty.

The suggestions come from the committed overlay and from what can be seen of
the hardware. On Linux, qwen3.5 at 32k, or 16k on a GPU with less than 8GB. On
a Mac, qwen3.8:27b-mlx at 32k, or qwen3.5 at 16k with less than 48GB of
memory. The budget suggestion is the window × 3 bytes, the pessimistic end for
a token, less 40% for the reply, tool results arriving mid-turn and the
request itself. A budget somebody typed stays put when the window changes. One
nobody changed follows it.

## Why the window is part of the model's name

The window used to be whatever the ollama server was started with:
`OLLAMA_CONTEXT_LENGTH` in somebody's shell on Linux, ollama's own default on a
Mac. Nothing in the repository recorded it, so `context_budget` was sized
against a number nobody could see. When the two disagree, a budget larger than
the window hands the model server the job of deciding what to drop from a long
conversation, and it drops oldest first and silently, without the order
outturn's own trim keeps. See "Compaction" in AGENTS.md.

So with ollama the script serves a tag of its own, `outturn/qwen3.5-ctx32768`,
made from the pulled model with `num_ctx` set to the answer. A model's
`num_ctx` overrides the server's `OLLAMA_CONTEXT_LENGTH`, so the window is the
answered one however the server was started, and the name says what it is
wherever it shows up. The tag keeps the base model's other parameters, and
is remade on every run, which takes a tenth of a second. After loading it, the
script reads the window back from `/api/ps` and warns if it is not the one
answered.

Asking for the window per request was the other way, but ollama's
OpenAI-compatible endpoint ignores `num_ctx`, and teaching the gateway an
ollama-only field would break the rule that it speaks protocols, not vendors.

The tag is what the usage ledger records as the model. That matters to nobody
locally, and a deployment that bills from the ledger is not running ollama.

With a model server that is not ollama, the model is used as named and the
window is that server's business. The budget is still yours to size against
it.

## llama.cpp instead of ollama

Both speak the OpenAI protocol, so the platform needs nothing different; what
differs is how each turns a model's raw output into that protocol, and that
difference is the reason for the choice.

ollama sends a tool call only once the model has finished writing it, whole --
by design, since it cannot tell a call has begun until it parses
([ollama#10415](https://github.com/ollama/ollama/pull/10415)). So the reader
sees nothing while a call is written, and a thought's clock runs on through it
until a second of quiet stops it. And a model's tool calls are only as good as
ollama's parser for that model: one we tried lost the call altogether on any
turn after the first that used a tool, with thinking on -- measured against
ollama directly, not one call in ten came back, while thinking off or a first
turn returned every one.

llama-server with `--jinja` uses the model's own chat template and a parser
that works on partial output, and streams a call as it is written, name first
([llama.cpp#12379](https://github.com/ggml-org/llama.cpp/pull/12379)) -- the
shape OpenAI and Anthropic send, which the "Preparing…" card is built for.

With `OUTTURN_DEV_SERVER=llama.cpp`:

- The model is a Hugging Face GGUF as `organisation/repository:quantization`,
  for instance `unsloth/Qwen3.5-9B-GGUF:Q4_K_M`, the same model and
  quantization as ollama's `qwen3.5`. llama-server downloads
  and caches it on first use. MLX builds are ollama's: llama.cpp runs GGUF on
  Metal.
- The window is llama-server's `-c`, and the model is served under an alias in
  the same shape as ollama's tag, `outturn/Qwen3.5-9B-GGUF-Q4_K_M-ctx32768`,
  which is what the cluster asks for.
- On a Mac, `scripts/dev-mac.sh` starts llama-server in the background and
  keeps its process id and log beside the answers
  (`k8s/overlays/local/.llama-server.pid` and `.llama-server.log`). A later run
  with different answers restarts a server it started; one somebody else
  started is named and left alone. On Linux, starting it is yours, as with
  ollama, and the script prints the command.
- The generated overlay points the gateway at it. The base overlays name
  ollama's port, so another server's address has to be said, translated to one
  a pod reaches: `host.docker.internal` on a Mac, the Docker bridge on Linux.

`scripts/dev-setup.sh --show` prints the command it starts llama-server with
and the address the gateway uses.

## What the answers cannot set

What the tag cannot set is anything server-wide. On Linux, ollama must listen
beyond loopback for the cluster to reach it (`OLLAMA_HOST=0.0.0.0`), and
qwen3.5 at 32k fits an 8GB card only with the 8-bit KV cache
(`OLLAMA_KV_CACHE_TYPE=q8_0`, with `OLLAMA_FLASH_ATTENTION=1`). Those stay with
whoever starts the server.

## How the answers reach the cluster

`scripts/lib/dev.sh` already writes `k8s/overlays/.generated` for each run: the
base, plus whichever `--with` components were asked for. It now also patches
two values from the answers:

- `OUTTURN_DEFAULT_MODEL` on the API, to the served model
- `CONTEXT_BUDGET` on the `platform-settings` Job, which PUTs it as the
  operator's `context_budget` once the API is up

The committed overlays keep their own values as fallbacks, so
`kubectl apply -k k8s/overlays/local` and `skaffold dev -p mac` still work with
no answers file. The dev scripts always override them. There used to be a
`local-mac-small` overlay and a `--small` flag. They existed to say two
numbers, which are answers now.

A Job's template cannot change in place, so a deploy carrying a new budget is
refused while the last run's Job is still kept. The dev scripts remove a
finished `platform-settings` Job that holds a different budget before handing
over to skaffold. If you deploy some other way, `kubectl delete job
platform-settings` does the same.

The operator's level is deliberate. A value set on a workspace in the Settings
page still wins, so experimenting there is not undone by the next deploy.

## Where the file lives

Beside `dev-secrets.env`, so everything a clone generates for itself is in
one place. It is not per checkout, though. Run from a `git worktree`, the
scripts read and write the main checkout's copy, because the answers describe
the model server and every worktree talks to the same one. In a tree with no
git, such as a downloaded archive, or one that happens to sit inside some
other repository, it is that tree's own.

Without a terminal, in CI or through a pipe, nothing is asked. The run uses the
suggestions, says so, and writes nothing, so the first run at a terminal still
asks.

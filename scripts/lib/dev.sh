# Local development, shared by scripts/dev.sh and scripts/dev-mac.sh.
#
# Sourced, not run. What differs between the two is the base overlay and the
# model server; everything else -- components, the generated overlay, the
# cluster checks, the skaffold invocation -- is here, so a feature added for
# one machine reaches the other.

# Where the Control API listens. Shared with scripts/build.sh through the
# environment so the two cannot disagree about the port.
dev_rpc_port="${OUTTURN_SKAFFOLD_RPC_PORT:-50052}"

dev_components() {
  ls k8s/components | tr '\n' ' '
}

# Takes `--with a,b` and `--reconfigure` out of the arguments, and refuses a
# profile of the caller's own. Sets `dev_features` to the components asked
# for, `dev_reconfigure`, and `dev_args` to what is left for skaffold.
#
# A second -p cannot work: the scripts pass their own, skaffold takes the last
# rather than merging them, and the cluster comes up on whatever base that
# profile names. It then fails one turn at a time in the gateway's log, long
# after the thing that caused it.
dev_parse() {
  dev_features=()
  dev_args=()
  dev_reconfigure=false
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --with)
        if [[ -z "${2:-}" ]]; then
          echo "--with needs a component: $(dev_components)" >&2
          exit 2
        fi
        IFS=, read -r -a more <<<"$2"
        dev_features+=("${more[@]}")
        shift 2
        ;;
      --reconfigure)
        dev_reconfigure=true
        shift
        ;;
      -p | --profile | -p=* | --profile=*)
        echo "this script passes its own profile; a second -p replaces it." >&2
        echo "for extra services use: --with $(dev_components | tr ' ' ',' | sed 's/,$//')" >&2
        exit 2
        ;;
      *)
        dev_args+=("$1")
        shift
        ;;
    esac
  done

  # Named rather than assumed: a typo here would otherwise produce an overlay
  # kustomize refuses, and the error it gives names a generated path nobody
  # wrote.
  for feature in ${dev_features[@]+"${dev_features[@]}"}; do
    if [[ ! -d "k8s/components/$feature" ]]; then
      echo "no such component: $feature" >&2
      echo "available: $(dev_components)" >&2
      exit 2
    fi
  done
}

# Writes the overlay this run deploys: the base, plus whichever components
# were asked for, plus this machine's answers. Generated rather than committed
# because the alternative is one overlay per combination of base, components
# and machine, and nobody keeps those in step -- local-mac-small was one, and
# it existed to say two numbers.
#
# Needs dev_machine_load first, for the model and the budget.
#
# Under the repository rather than in /tmp because kustomize resolves
# `resources` and `components` relative to the file, and a path out of the
# tree cannot reach back into it.
dev_write_overlay() {
  local base=$1
  local generated=k8s/overlays/.generated
  mkdir -p "$generated"
  {
    echo "# Written by scripts/lib/dev.sh. Not committed: every run replaces it,"
    echo "# and the base, the component or the answers file is the thing to edit."
    echo "#"
    echo "# Do not delete it while skaffold is running. The dev loop renders this"
    echo "# path on every rebuild, so removing it fails each one on a missing"
    echo "# directory -- a long way from whatever removed it."
    echo "apiVersion: kustomize.config.k8s.io/v1beta1"
    echo "kind: Kustomization"
    echo
    echo "resources:"
    echo "  - ../$base"
    if [[ ${#dev_features[@]} -gt 0 ]]; then
      echo
      echo "components:"
      for feature in "${dev_features[@]}"; do
        echo "  - ../../components/$feature"
      done
    fi

    # The hosts those components need the gateway to be allowed to reach, as
    # one list.
    #
    # Written here rather than by each component because OUTTURN_INTERNAL_HOSTS
    # is a single variable and JSON Patch cannot append to a string: two
    # components each opening a host would overwrite one another, and the one
    # that lost would be unreachable with nothing said about why. So the
    # overlay carries the whole list, and a component only declares what it
    # needs in a file.
    #
    # Only what was asked for. A host is opened because somebody asked for the
    # component that needs it, which is the operator decision docs/egress.md
    # says this list is.
    local hosts=""
    for feature in ${dev_features[@]+"${dev_features[@]}"}; do
      [[ -f "k8s/components/$feature/internal-host" ]] || continue
      while read -r entry; do
        [[ -n "$entry" && "$entry" != \#* ]] || continue
        hosts="${hosts:+$hosts,}$entry"
      done <"k8s/components/$feature/internal-host"
    done
    echo
    echo "patches:"

    # This machine's answers (see dev_machine_load). Strategic merge, so env
    # is matched by name rather than by its position in ../local.
    echo "  # From $(dev_machine_file) -- change with scripts/dev-setup.sh --reconfigure."
    echo "  - patch: |"
    echo "      apiVersion: apps/v1"
    echo "      kind: Deployment"
    echo "      metadata:"
    echo "        name: outturn-api"
    echo "      spec:"
    echo "        template:"
    echo "          spec:"
    echo "            containers:"
    echo "              - name: api"
    echo "                env:"
    echo "                  - name: OUTTURN_DEFAULT_MODEL"
    echo "                    value: \"$dev_served_model\""
    echo "  - patch: |"
    echo "      apiVersion: batch/v1"
    echo "      kind: Job"
    echo "      metadata:"
    echo "        name: platform-settings"
    echo "      spec:"
    echo "        template:"
    echo "          spec:"
    echo "            containers:"
    echo "              - name: settings"
    echo "                env:"
    echo "                  - name: CONTEXT_BUDGET"
    echo "                    value: \"$dev_context_budget\""

    # Where the gateway reaches the model. The base overlays name ollama's
    # port, so another server has to say where it is -- translated from this
    # machine's address to one a pod can reach.
    local base_url
    base_url=$(dev_gateway_base_url)
    if [[ -n "$base_url" ]]; then
      echo "  - patch: |"
      echo "      apiVersion: apps/v1"
      echo "      kind: Deployment"
      echo "      metadata:"
      echo "        name: outturn-gateway"
      echo "      spec:"
      echo "        template:"
      echo "          spec:"
      echo "            containers:"
      echo "              - name: gateway"
      echo "                env:"
      echo "                  - name: OPENAI_BASE_URL"
      echo "                    value: \"$base_url\""
    fi

    if [[ -n "$hosts" ]]; then
      # Both tiers, because both halves of the decision read it: the gateway
      # to decide whether a connection may go out, and the API to decide
      # whether a rule may name the host at all.
      for tier in gateway api; do
        echo "  - target:"
        echo "      kind: Deployment"
        echo "      name: outturn-$tier"
        echo "    patch: |"
        echo "      apiVersion: apps/v1"
        echo "      kind: Deployment"
        echo "      metadata:"
        echo "        name: outturn-$tier"
        echo "      spec:"
        echo "        template:"
        echo "          spec:"
        echo "            containers:"
        echo "              - name: $tier"
        echo "                env:"
        echo "                  - name: OUTTURN_INTERNAL_HOSTS"
        echo "                    value: \"$hosts\""
      done
    fi
  } >"$generated/kustomization.yaml"
}

# Before anything slow: finding out after a build or a model pull that there
# was nowhere to deploy is an evening gone.
dev_require_cluster() {
  local hint=$1
  if ! kubectl cluster-info >/dev/null 2>&1; then
    echo "no cluster reachable from kubectl (context: $(kubectl config current-context 2>/dev/null || echo none))" >&2
    echo "$hint" >&2
    exit 1
  fi

  # The trap worth catching before it bites: skaffold decides whether to push
  # by guessing from the kube-context name, and a local cluster under a name
  # it does not recognize means four images pushed to a registry. Diagnosed
  # and explained; nothing is changed, because which registry is right is not
  # this script's call. See .agents/skills/onboarding/SKILL.md.
  local context
  context=$(kubectl config current-context 2>/dev/null || echo "")
  case "$context" in
    kind-* | docker-desktop | minikube | colima | k3d-*) ;;
    *)
      echo "warning: skaffold does not recognize the context '$context' as local," >&2
      echo "so it will PUSH images rather than loading them into the cluster." >&2
      echo "If that is not what you want, either rename the context or set:" >&2
      echo "  skaffold config set --kube-context '$context' local-cluster true" >&2
      echo >&2
      ;;
  esac
}

# This machine's answers: which model, the window it runs at, and how much of
# a conversation outturn sends it. Asked once by scripts/dev-setup.sh and kept
# in a gitignored file, because they are facts about the machine rather than
# about the code, and an overlay per machine is what they replaced.
#
# Where the file lives. Beside dev-secrets.env, so everything this clone
# generated for itself is in one place -- but in the main checkout's tree even
# when run from a `git worktree`, since the answers describe the model server
# and every worktree talks to the same one. Without git (a downloaded archive)
# or when this tree only happens to sit inside some other repository, it is
# this tree's own.
dev_machine_file() {
  local root=. common
  if common=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null) &&
    [[ "$(git rev-parse --show-toplevel 2>/dev/null)" == "$(pwd -P)" &&
      "$(basename "$common")" == .git && -f "$(dirname "$common")/scripts/lib/dev.sh" ]]; then
    root=$(dirname "$common")
  fi
  echo "$root/k8s/overlays/local/dev-machine.env"
}

# The base overlay this machine builds on. Only the address of ollama differs
# between the two, and that follows from the OS rather than from a choice.
dev_machine_base() {
  if [[ "$(uname -s)" == Darwin ]]; then echo local-mac; else echo local; fi
}

# window tokens x 3 bytes -- the pessimistic end, as the setting's description
# says -- less 40% for the reply, tool results arriving mid-turn and the request
# itself. Rounded to 5000 so the number reads as the estimate it is.
dev_budget_for() {
  local budget=$(( ($1 * 18 / 10 + 2500) / 5000 * 5000 ))
  (( budget >= 1000 )) || budget=1000
  echo "$budget"
}

# Suggestions, not answers: what the committed overlay names, adjusted for
# what can be seen of the hardware. Sets `dev_hint` to say why, so a person
# accepting a default knows what it was based on.
dev_machine_defaults() {
  local base
  base=$(dev_machine_base)
  dev_model=$(awk '/name: OUTTURN_DEFAULT_MODEL/ { getline; gsub(/.*value: "|"$/, ""); print; exit }' \
    "k8s/overlays/$base/kustomization.yaml")
  dev_context_window=32768
  dev_server=ollama
  dev_ollama_url=http://localhost:11434
  dev_llama_url=http://localhost:8080
  dev_other_url=""
  dev_hint=""

  if [[ "$base" == local-mac ]]; then
    # qwen3.8:27b-mlx is 18-23GB loaded, which wants a 64GB machine. Below
    # 48GB the smaller model of the same family, at half the window.
    local gib=$(( $(sysctl -n hw.memsize 2>/dev/null || echo 0) / 1073741824 ))
    if (( gib > 0 && gib < 48 )); then
      dev_model=qwen3.5
      dev_context_window=16384
      dev_hint="${gib}GB of memory, so the smaller model"
    elif (( gib > 0 )); then
      dev_hint="${gib}GB of memory"
    fi
  else
    # qwen3.5 at 32k fits an 8GB card with ollama's 8-bit KV cache
    # (OLLAMA_KV_CACHE_TYPE=q8_0). Less than that, half the window.
    local mib
    # `|| true`: without nvidia-smi the pipeline fails, and under pipefail
    # that ends the script silently rather than reaching the else below.
    mib=$(nvidia-smi --query-gpu=memory.total --format=csv,noheader,nounits 2>/dev/null | head -1 | tr -d ' ' || true)
    if [[ "$mib" =~ ^[0-9]+$ ]]; then
      if (( mib < 8000 )); then
        dev_context_window=16384
        dev_hint="$((mib / 1024))GB GPU, so half the window"
      else
        dev_hint="$((mib / 1024))GB GPU"
      fi
    else
      dev_hint="no NVIDIA GPU found"
    fi
  fi
  dev_context_budget=$(dev_budget_for "$dev_context_window")
}

# Sets dev_model, dev_context_window, dev_context_budget, dev_server and the
# server's address: the file's values over the defaults. Read line by line
# rather than sourced, so a stray line in a hand-edited file is ignored rather
# than run.
#
# A file written before there was a choice of server has no
# OUTTURN_DEV_SERVER, and meant ollama wherever it named one and some other
# server where it left the address empty -- so that is what it still means.
dev_machine_load() {
  dev_machine_defaults
  local file key value server=""
  file=$(dev_machine_file)
  if [[ -f "$file" ]]; then
    while IFS='=' read -r key value; do
      case "$key" in
        OUTTURN_DEV_MODEL) dev_model=$value ;;
        OUTTURN_DEV_CONTEXT_WINDOW) dev_context_window=$value ;;
        OUTTURN_DEV_CONTEXT_BUDGET) dev_context_budget=$value ;;
        OUTTURN_DEV_SERVER) server=$value ;;
        OUTTURN_DEV_OLLAMA_URL) dev_ollama_url=$value ;;
        OUTTURN_DEV_LLAMA_URL) dev_llama_url=$value ;;
        OUTTURN_DEV_OTHER_URL) dev_other_url=$value ;;
      esac
    done <"$file"
    if [[ -n "$server" ]]; then
      dev_server=$server
    elif [[ -z "$dev_ollama_url" ]]; then
      dev_server=other
    fi
  fi
  dev_served_model=$(dev_served_model_for "$dev_model" "$dev_context_window")
}

# Where the gateway, in the cluster, reaches the model server. Empty for
# ollama, whose address the base overlay already names, and for another server
# given no address of its own. Otherwise this machine's address as a pod sees
# it: a Mac's loopback is host.docker.internal from inside Docker Desktop or
# colima, and a Linux host is the Docker bridge the ../local overlay already
# uses. An address on another machine passes through unchanged.
#
# Another server's address lives here, in this machine's answers, because it is
# this machine's: written into the committed overlay instead, it is a change
# every clone either carries or keeps reverting.
dev_gateway_base_url() {
  local url
  case "${dev_server:-}" in
    llama.cpp) url=$dev_llama_url ;;
    other) url=$dev_other_url ;;
    *) return 0 ;;
  esac
  [[ -n "$url" ]] || return 0
  local host
  if [[ "$(dev_machine_base)" == local-mac ]]; then host=host.docker.internal; else host=172.18.0.1; fi
  echo "$url" | sed -E "s#//(localhost|127\.0\.0\.1)([:/]|\$)#//$host\2#"
}

# The model the cluster asks for. With ollama, a tag of our own derived from
# the one pulled, carrying the window as num_ctx -- so the window is the one
# answered here whatever the server was started with, and the name says what
# it is wherever it shows up. Without ollama, the model as answered, and the
# window is the server's business.
#
# llama.cpp gets the same kind of name, as the alias llama-server is started
# with: the GGUF's file name without its organization, so a log line says
# which model and which window rather than a Hugging Face path.
dev_served_model_for() {
  case "${dev_server:-ollama}" in
    ollama) echo "outturn/$(echo "$1" | tr ':' '-')-ctx$2" ;;
    llama.cpp) echo "outturn/$(echo "${1##*/}" | tr ':' '-')-ctx$2" ;;
    *) echo "$1" ;;
  esac
}

# What each server wants as a model name, suggested when the server changes
# and the model answered was the other kind of name.
dev_llama_default_model=unsloth/Qwen3.5-9B-GGUF:Q4_K_M

# Asks, with the current values as the suggestions, and writes the file.
dev_machine_ask() {
  local file answer derived
  file=$(dev_machine_file)
  derived=$(dev_budget_for "$dev_context_window")

  echo "Local model settings for this machine, saved to $file."
  echo "Enter keeps the suggestion.${dev_hint:+ ($dev_hint.)}"
  echo

  while :; do
    read -r -p "Model server: ollama, llama.cpp or other [$dev_server]: " answer
    answer=${answer:-$dev_server}
    case "$answer" in ollama | llama.cpp | other) break ;; esac
    echo "  ollama, llama.cpp or other"
  done
  # A model named for one server is a wrong suggestion for another: ollama's
  # names have no organization, llama.cpp's are a Hugging Face GGUF with a
  # quantization after the colon.
  if [[ "$answer" == llama.cpp && "$dev_model" != */*:* ]]; then
    dev_model=$dev_llama_default_model
  elif [[ "$answer" == ollama && "$dev_model" == */*:* ]]; then
    dev_machine_defaults
  fi
  dev_server=$answer

  local naming
  case "$dev_server" in
    ollama) naming="as ollama names it" ;;
    llama.cpp) naming="a Hugging Face GGUF, organization/repository:quantization" ;;
    *) naming="as the server names it" ;;
  esac
  while :; do
    read -r -p "Model, $naming [$dev_model]: " answer
    answer=${answer:-$dev_model}
    [[ "$answer" =~ ^[A-Za-z0-9._:/-]+$ ]] && break
    echo "  letters, digits and ._:/- only"
  done
  dev_model=$answer

  while :; do
    read -r -p "Context window, in tokens [$dev_context_window]: " answer
    answer=${answer:-$dev_context_window}
    [[ "$answer" =~ ^[0-9]+$ ]] && (( answer >= 2048 )) && break
    echo "  a number of tokens, 2048 or more"
  done
  # A budget nobody changed follows the window; one somebody chose stays.
  if [[ "$dev_context_budget" == "$derived" ]]; then
    dev_context_budget=$(dev_budget_for "$answer")
  fi
  dev_context_window=$answer

  while :; do
    read -r -p "Conversation budget, in bytes -- window x 3, less 40% [$dev_context_budget]: " answer
    answer=${answer:-$dev_context_budget}
    [[ "$answer" =~ ^[0-9]+$ ]] && (( answer >= 1000 )) && break
    echo "  a number of bytes, 1000 or more"
  done
  dev_context_budget=$answer

  case "$dev_server" in
    ollama)
      read -r -p "ollama, from this machine [${dev_ollama_url:-http://localhost:11434}]: " answer
      answer=${answer:-${dev_ollama_url:-http://localhost:11434}}
      dev_ollama_url=${answer%/}
      ;;
    llama.cpp)
      read -r -p "llama-server, from this machine [$dev_llama_url]: " answer
      answer=${answer:-$dev_llama_url}
      dev_llama_url=${answer%/}
      dev_ollama_url=""
      ;;
    *)
      read -r -p "The server, from this machine; '-' for the overlay's own address [${dev_other_url:--}]: " answer
      answer=${answer:-${dev_other_url:--}}
      if [[ "$answer" == - ]]; then dev_other_url=""; else dev_other_url=${answer%/}; fi
      dev_ollama_url=""
      ;;
  esac

  cat >"$file" <<EOF
# This machine's answers for local development, written by
# scripts/dev-setup.sh and read by scripts/dev.sh and scripts/dev-mac.sh.
# Not committed. Edit it, or run scripts/dev-setup.sh --reconfigure.
#
# See docs/local-development.md for what each one drives.

# ollama, llama.cpp, or other: some OpenAI-compatible server, at
# OUTTURN_DEV_OTHER_URL or else the address the base overlay names, which
# nothing here starts or checks.
OUTTURN_DEV_SERVER=$dev_server

# Pulled if missing. With ollama, served as a tag of its own carrying the
# window below; with llama.cpp, a Hugging Face GGUF that llama-server
# downloads, served under an alias carrying the window. Either name is what
# agents with no model of their own use.
OUTTURN_DEV_MODEL=$dev_model

# Tokens. Set on the model through that tag, so it is this number whatever
# OLLAMA_CONTEXT_LENGTH the server was started with.
OUTTURN_DEV_CONTEXT_WINDOW=$dev_context_window

# Bytes of conversation sent per request: the operator's context_budget.
OUTTURN_DEV_CONTEXT_BUDGET=$dev_context_budget

# Where this machine reaches ollama. Empty if the model server is something
# else, in which case the model is used as named and the window is its own.
OUTTURN_DEV_OLLAMA_URL=$dev_ollama_url

# Where this machine reaches llama-server, when that is the server.
OUTTURN_DEV_LLAMA_URL=$dev_llama_url

# Where this machine reaches another server, when that is the server. Empty
# means the address the base overlay names.
OUTTURN_DEV_OTHER_URL=$dev_other_url
EOF
  echo
  echo "wrote $file"
  dev_served_model=$(dev_served_model_for "$dev_model" "$dev_context_window")
}

# Asks if there are no answers yet or if asked to, then loads them. Without a
# terminal it never asks -- a run in CI or through a pipe would otherwise hang
# on a question nobody can see -- and says it is using the suggestions.
dev_machine_setup() {
  local reconfigure=$1 file
  file=$(dev_machine_file)
  dev_machine_load
  if [[ -f "$file" && "$reconfigure" != true ]]; then
    return
  fi
  if [[ ! -t 0 ]]; then
    echo "no terminal to ask on; using the suggested model settings" \
      "($dev_model, ${dev_context_window} tokens, ${dev_context_budget} bytes)." >&2
    echo "run scripts/dev-setup.sh to choose and keep them." >&2
    return
  fi
  dev_machine_ask
}

# Readies whichever server this machine answered, before skaffold.
dev_model_server_prepare() {
  case "$dev_server" in
    ollama) dev_ollama_prepare ;;
    llama.cpp) dev_llamacpp_prepare ;;
    *) echo "model server is not one this starts; asking it for $dev_model as named" ;;
  esac
}

# Where a llama-server this script started keeps its process id and its log:
# per clone, beside the answers, so two clones do not stop each other's.
dev_llamacpp_pidfile() { echo "$(dirname "$(dev_machine_file)")/.llama-server.pid"; }
dev_llamacpp_log() { echo "$(dirname "$(dev_machine_file)")/.llama-server.log"; }

# The command that serves this machine's answers.
#
# --jinja because that is what gives llama-server the model's own chat
# template and its tool-call parser, which streams a call as it is written and
# is the reason to use this over ollama; without it tools are not offered at
# all. The alias is the name the cluster asks for.
dev_llamacpp_command() {
  local port
  port=$(echo "$dev_llama_url" | sed -nE 's#.*:([0-9]+)(/.*)?$#\1#p')
  echo "llama-server -hf $dev_model --jinja -c $dev_context_window --alias $dev_served_model --host 127.0.0.1 --port ${port:-8080}"
}

# What a running llama-server is serving, as "alias window", or nothing.
dev_llamacpp_serving() {
  local alias window
  alias=$(curl -sf "$dev_llama_url/v1/models" | sed -nE 's/.*"id":"([^"]*)".*/\1/p' | head -1)
  window=$(curl -sf "$dev_llama_url/props" | sed -nE 's/.*"n_ctx":([0-9]+).*/\1/p' | head -1)
  [[ -n "$alias" ]] && echo "$alias ${window:-?}"
}

# Makes llama-server serve this machine's answers.
#
# One already serving them is used as it is. One this script started for
# different answers is stopped and started again, since that is what changing
# an answer means; one somebody else started is left alone and named, because
# stopping a server nobody asked this to own is not a setup step.
#
# Starting is the caller's, as with ollama: dev_llamacpp_start, where defined.
dev_llamacpp_prepare() {
  local serving pidfile
  pidfile=$(dev_llamacpp_pidfile)
  serving=$(dev_llamacpp_serving || true)
  if [[ "$serving" == "$dev_served_model $dev_context_window" ]]; then
    echo "llama-server already serving $dev_served_model at $dev_context_window tokens"
    return
  fi
  if [[ -n "$serving" ]]; then
    if [[ -f "$pidfile" ]] && kill -0 "$(cat "$pidfile")" 2>/dev/null; then
      echo "llama-server is serving $serving; restarting it for $dev_served_model at $dev_context_window"
      kill "$(cat "$pidfile")"
      for _ in $(seq 30); do curl -sf "$dev_llama_url/health" >/dev/null || break; sleep 1; done
    else
      echo "something else on $dev_llama_url is serving $serving, not $dev_served_model at $dev_context_window." >&2
      echo "stop it, or answer another address in: scripts/dev-setup.sh --reconfigure" >&2
      exit 1
    fi
  fi

  if ! command -v llama-server >/dev/null; then
    echo "llama-server is not installed. Either:" >&2
    echo "  brew install llama.cpp" >&2
    echo "  or a release from https://github.com/ggml-org/llama.cpp/releases, on your PATH" >&2
    exit 1
  fi
  if declare -F dev_llamacpp_start >/dev/null; then
    dev_llamacpp_start
  else
    echo "nothing is serving $dev_served_model on $dev_llama_url. Start it with:" >&2
    echo "  $(dev_llamacpp_command)" >&2
    exit 1
  fi

  # A first run downloads the model, which is minutes rather than seconds;
  # the log says how far along it is, so the wait is shown rather than silent.
  echo "waiting for llama-server (a first run downloads $dev_model; see $(dev_llamacpp_log))"
  local waited=0
  until curl -sf "$dev_llama_url/health" >/dev/null; do
    if [[ -f "$pidfile" ]] && ! kill -0 "$(cat "$pidfile")" 2>/dev/null; then
      echo "llama-server stopped. The end of its log:" >&2
      tail -20 "$(dev_llamacpp_log)" >&2
      exit 1
    fi
    sleep 5
    waited=$((waited + 5))
    # llama-server logs nothing while it downloads, so say how much has
    # arrived in Hugging Face's cache instead, which is where it goes.
    if (( waited % 60 == 0 )); then
      local repo=${dev_model%%:*} got
      got=$(du -sh "$HOME/.cache/huggingface/hub/models--${repo//\//--}" 2>/dev/null | cut -f1)
      echo "  still loading after ${waited}s${got:+, $got downloaded}"
    fi
  done
  echo "llama-server serving $(dev_llamacpp_serving)"
}

# Makes the served model exist and loads it. Before skaffold, so the first
# reply is not also the one that waits for the weights -- and so a window
# that did not take is said now rather than inferred from a trimmed reply.
#
# Calls dev_ollama_start, if the caller defined one, when nothing answers:
# on a Mac the script owns starting ollama, on Linux whoever runs it does.
dev_ollama_prepare() {
  local url=$dev_ollama_url
  if [[ -z "$url" ]]; then
    echo "model server is not ollama; asking it for $dev_model as named"
    return
  fi

  if ! curl -sf "$url/api/version" >/dev/null; then
    if declare -F dev_ollama_start >/dev/null; then
      dev_ollama_start "$url"
    fi
    if ! curl -sf "$url/api/version" >/dev/null; then
      echo "nothing answers on $url. Start ollama, or answer '-' for it in:" >&2
      echo "  scripts/dev-setup.sh --reconfigure" >&2
      exit 1
    fi
  fi

  if ! curl -sf "$url/api/show" -d "{\"model\":\"$dev_model\"}" >/dev/null; then
    echo "pulling $dev_model"
    if command -v ollama >/dev/null; then
      OLLAMA_HOST=$url ollama pull "$dev_model"
    else
      curl -sf "$url/api/pull" -d "{\"model\":\"$dev_model\",\"stream\":false}" >/dev/null
    fi
  fi

  # Every run rather than when missing: it takes a tenth of a second, and it
  # keeps the tag on the weights just pulled rather than the ones it was
  # first made from. The tag keeps the base's other parameters.
  if ! curl -sf "$url/api/create" -d "{\"model\":\"$dev_served_model\",\"from\":\"$dev_model\",\"parameters\":{\"num_ctx\":$dev_context_window},\"stream\":false}" >/dev/null; then
    echo "ollama would not make $dev_served_model from $dev_model" >&2
    exit 1
  fi

  echo "loading $dev_served_model"
  curl -sf "$url/api/generate" -d "{\"model\":\"$dev_served_model\",\"keep_alive\":\"30m\"}" >/dev/null

  # What the server actually loaded. A mismatch is loud but not fatal: the
  # trim still keeps turns inside the budget, only less well than it could.
  local loaded
  loaded=$(curl -sf "$url/api/ps" |
    sed -n 's|.*"name":"'"$dev_served_model"':latest"[^}]*}[^}]*"context_length":\([0-9]*\).*|\1|p')
  if [[ -z "$loaded" ]]; then
    echo "could not read the loaded window back from $url/api/ps; assuming $dev_context_window"
  elif [[ "$loaded" != "$dev_context_window" ]]; then
    echo "warning: ollama loaded $dev_served_model with a $loaded-token window, not $dev_context_window." >&2
    echo "context_budget is sized for $dev_context_window; see docs/local-development.md." >&2
  fi
}

# The budget reaches the cluster through a Job, and a Job's template cannot be
# changed in place: for as long as the last run is kept, a deploy carrying a
# new budget is refused, and the loop fails on the first build after the
# answer changed. A finished Job is only a record of a PUT, so one holding a
# different number is removed and the deploy makes it again.
dev_clear_stale_settings_job() {
  local deployed
  deployed=$(kubectl get job platform-settings \
    -o jsonpath='{.spec.template.spec.containers[0].env[?(@.name=="CONTEXT_BUDGET")].value}' 2>/dev/null) || return 0
  if [[ -n "$deployed" && "$deployed" != "$dev_context_budget" ]]; then
    echo "context_budget changes from $deployed to $dev_context_budget; clearing the last platform-settings Job"
    kubectl delete job platform-settings >/dev/null
  fi
}

# The dev loop, with nothing automatic: the Control API starts a build instead
# (scripts/build.sh), so a file save does not start one in the background
# while you are reading something.
dev_skaffold() {
  echo "Control API on :$dev_rpc_port -- trigger a build and deploy with scripts/build.sh"
  exec skaffold dev -p generated \
    --auto-build=false --auto-deploy=false --auto-sync=false \
    --rpc-http-port="$dev_rpc_port" \
    ${dev_args[@]+"${dev_args[@]}"}
}

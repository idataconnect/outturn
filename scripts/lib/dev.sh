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

# Takes `--with a,b` out of the arguments, and refuses a profile of the
# caller's own. Sets `dev_features` to the components asked for and
# `dev_args` to what is left for skaffold.
#
# A second -p cannot work: the scripts pass their own, skaffold takes the last
# rather than merging them, and the cluster comes up on whatever base that
# profile names. It then fails one turn at a time in the gateway's log, long
# after the thing that caused it.
dev_parse() {
  dev_features=()
  dev_args=()
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
# were asked for. Generated rather than committed because the alternative is
# one overlay per combination of base and components, and nobody keeps those
# in step.
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
    echo "# and the base or the component is the thing to edit."
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
    if [[ -n "$hosts" ]]; then
      echo
      echo "patches:"
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
  # it does not recognise means four images pushed to a registry. Diagnosed
  # and explained; nothing is changed, because which registry is right is not
  # this script's call. See .agents/skills/onboarding/SKILL.md.
  local context
  context=$(kubectl config current-context 2>/dev/null || echo "")
  case "$context" in
    kind-* | docker-desktop | minikube | colima | k3d-*) ;;
    *)
      echo "warning: skaffold does not recognise the context '$context' as local," >&2
      echo "so it will PUSH images rather than loading them into the cluster." >&2
      echo "If that is not what you want, either rename the context or set:" >&2
      echo "  skaffold config set --kube-context '$context' local-cluster true" >&2
      echo >&2
      ;;
  esac
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

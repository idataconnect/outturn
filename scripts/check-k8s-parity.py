#!/usr/bin/env python3
"""Fails if k8s/deploy and k8s/base disagree where they must not.

The two are separate on purpose -- they differ in nearly every Deployment's
env, resources and replicas, and k8s/deploy/kustomization.yaml says why one is
not an overlay of the other. A shared kustomize Component was the alternative,
and was not taken: the fields that must match are scattered through Deployments
that are otherwise different, so a Component would be patches over both files
rather than a file both include, and a patch that silently stops applying to
one of them is the very drift this is here to catch. A check leaves both
renders exactly as they are and says out loud when they part.

What must match is what the local cluster is there to teach about production:
how long a pod is given to finish its turns, the runtime's memory limit that
admission is tuned against, where the probes look, and the security context.

Run from anywhere; needs kubectl (for kustomize) and PyYAML.
"""

import json
import pathlib
import subprocess
import sys

import yaml

ROOT = pathlib.Path(__file__).resolve().parent.parent
TIERS = ("outturn-api", "outturn-gateway", "outturn-runtime")


def deployments(path):
    out = subprocess.check_output(["kubectl", "kustomize", str(ROOT / path)], text=True)
    return {
        o["metadata"]["name"]: o
        for o in yaml.safe_load_all(out)
        if o and o.get("kind") == "Deployment" and o["metadata"]["name"] in TIERS
    }


def must_match(d):
    pod = d["spec"]["template"]["spec"]
    c = pod["containers"][0]
    fields = {
        "terminationGracePeriodSeconds": pod.get("terminationGracePeriodSeconds"),
        "automountServiceAccountToken": pod.get("automountServiceAccountToken"),
        "pod securityContext": pod.get("securityContext"),
        "container securityContext": c.get("securityContext"),
        "ports": c.get("ports"),
    }
    for probe in ("startupProbe", "readinessProbe", "livenessProbe"):
        fields[probe] = c.get(probe)
    if d["metadata"]["name"] == "outturn-runtime":
        fields["memory limit"] = c.get("resources", {}).get("limits", {}).get("memory")
    return fields


def main():
    base, deploy = deployments("k8s/base"), deployments("k8s/deploy")
    failed = False
    for tier in TIERS:
        if tier not in base or tier not in deploy:
            print(f"{tier}: missing from {'k8s/base' if tier not in base else 'k8s/deploy'}")
            failed = True
            continue
        want, got = must_match(base[tier]), must_match(deploy[tier])
        for field in want:
            if want[field] != got[field]:
                print(
                    f"{tier} {field}: k8s/base has {json.dumps(want[field])}, "
                    f"k8s/deploy has {json.dumps(got[field])}"
                )
                failed = True
    if failed:
        sys.exit(1)
    print(f"k8s/base and k8s/deploy agree on {', '.join(TIERS)}")


if __name__ == "__main__":
    main()

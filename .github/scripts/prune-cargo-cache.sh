#!/usr/bin/env bash
# Deletes all but the newest cargo cache of each name saved by main. A name is a key prefix
# such as `Linux-conformance-`; only keys made of the prefix and a lockfile hash belong to it,
# so `Linux-conformance-` leaves `Linux-conformance-skia-…` alone.
#   prune-cargo-cache.sh <prefix>...
# Needs `gh` with GH_TOKEN (`actions: write`) and GITHUB_REPOSITORY.
set -euo pipefail

[ $# -gt 0 ] || { echo "usage: prune-cargo-cache.sh <prefix>..." >&2; exit 2; }
repo=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is not set}
for prefix in "$@"; do
  ids=$(gh cache list --repo "$repo" --ref refs/heads/main --key "$prefix" \
    --sort created_at --order desc --limit 100 --json id,key \
    --jq "[.[] | select(.key | test(\"^${prefix}[0-9a-f]{64}\$\"))] | .[1:] | .[].id")
  for id in $ids; do
    gh cache delete "$id" --repo "$repo"
    echo "$prefix: deleted cache $id"
  done
  [ -n "$ids" ] || echo "$prefix: nothing to prune"
done

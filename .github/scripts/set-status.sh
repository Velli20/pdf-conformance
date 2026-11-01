#!/usr/bin/env bash
# Sets the `conformance/reads` commit status on a Safe-PDF commit, linking to this run.
#   set-status.sh <pending|success|failure|error> <description>
# Needs `gh` with GH_TOKEN (statuses: write on Safe-PDF), SAFE_PDF_REPOSITORY, SHA and RUN_URL.
set -euo pipefail

state=${1:?usage: set-status.sh <state> <description>}
description=${2:?usage: set-status.sh <state> <description>}
gh api --method POST "repos/${SAFE_PDF_REPOSITORY:?}/statuses/${SHA:?}" \
  -f state="$state" \
  -f context="conformance/reads" \
  -f description="$description" \
  -f target_url="${RUN_URL:?}" >/dev/null
echo "conformance/reads: $state ($description)"

#!/usr/bin/env bash
# Appends the head of a report's TRIAGE.md and its largest failure clusters to the job summary.
#   triage-summary.sh <TRIAGE.md> [<report published on Pages>]
set -euo pipefail

triage=${1:?usage: triage-summary.sh <TRIAGE.md> [<report>]}
report=${2:-}
[ -f "$triage" ] || exit 0
{
  sed '/^## Failure clusters/,$d' "$triage"
  if [ -n "$report" ] && [ "${GITHUB_REF:-}" = refs/heads/main ]; then
    owner=$(echo "${GITHUB_REPOSITORY_OWNER:?}" | tr '[:upper:]' '[:lower:]')
    echo "Viewer (published when the run finishes): https://${owner}.github.io/${GITHUB_REPOSITORY#*/}/${report}/"
    echo
  fi
  echo "## Largest failure clusters"
  grep '^### ' "$triage" | head -25 | sed 's/^### /- /'
} >> "${GITHUB_STEP_SUMMARY:?}"

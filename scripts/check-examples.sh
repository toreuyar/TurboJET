#!/usr/bin/env bash
# Explicit optional example/distribution suite. It may read shipped workflows/
# packages and examples/ assets and is never part of the default asset-free job. The local
# full-SDLC regression is opt-in: when TJ_SDLC_GRAPH or TJ_RUN_LOCAL_SDLC=1 is
# supplied, a missing/absent local graph is a BLOCKED failure rather than a
# silent pass.
set -uo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
status=0

bash "$root/scripts/checks.sh" examples || status=$?

package="${TJ_SDLC_PACKAGE:-$root/config/local/full-sdlc.package}"
if [[ -n "${TJ_SDLC_PACKAGE:-}" || "${TJ_RUN_LOCAL_SDLC:-0}" == 1 || -f "$package/workflow.smg.json" ]]; then
  echo "== opt-in local SDLC operational regression =="
  bash "$root/scripts/check-sdlc-regression.sh" || status=$?
else
  echo "OPTIONAL local SDLC regression BLOCKED: no local package at $package"
  echo "Set TJ_SDLC_PACKAGE to an explicit package or run scripts/import-local-sdlc.sh derive."
fi

exit "$status"

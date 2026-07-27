#!/usr/bin/env bash
set -euo pipefail

if ! command -v lsof >/dev/null 2>&1; then
  echo "error: lsof is required to verify local service-port ownership" >&2
  exit 1
fi

blocked=0
for port in 3000 5432 6379; do
  listeners="$(lsof -nP -iTCP:"${port}" -sTCP:LISTEN 2>/dev/null || true)"
  if [[ -n "${listeners}" ]]; then
    echo "error: TCP port ${port} is already owned; refusing to start the Playbooks stack" >&2
    printf '%s\n' "${listeners}" >&2
    blocked=1
  fi
done

if [[ "${blocked}" -ne 0 ]]; then
  echo "Stop the listed listeners or use an explicitly isolated service stack before continuing." >&2
  exit 1
fi

echo "Playbooks local ports are unowned: 3000, 5432, 6379"

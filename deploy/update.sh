#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
resolve_role "${1:-}"
validate_config
export_compose_ids
git_owner="${SUDO_USER:-$(id -un)}"
if [[ "$(id -u)" -eq 0 && "$git_owner" != root ]]; then
    runuser -u "$git_owner" -- git -C "$APP_DIR" pull --ff-only
else
    git -C "$APP_DIR" pull --ff-only
fi
compose build
compose config --quiet
compose up -d --remove-orphans
compose ps

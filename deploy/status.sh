#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
resolve_role "${1:-}"
export_compose_ids
compose ps
compose exec -T redis redis-cli ping

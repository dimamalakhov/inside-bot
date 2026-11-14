#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
resolve_role "${1:-}"
validate_config
export_compose_ids
compose up -d
compose ps

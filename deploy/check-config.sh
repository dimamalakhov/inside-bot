#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
resolve_role "${1:-}"
validate_config
info "обязательные параметры роли $ROLE заполнены"

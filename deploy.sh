#!/usr/bin/env bash
# Publish types.cartercole.net to S3 + CloudFront + Route 53.
# Idempotent: provisioning and re-publishing are the same command.
set -euo pipefail

HOST="types.cartercole.net"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PUBLISH="$HOME/.claude/skills/publish-static-site/scripts/publish.py"

if [[ ! -f "$PUBLISH" ]]; then
  echo "publish.py not found at $PUBLISH" >&2
  exit 1
fi

exec python3 "$PUBLISH" "$HOST" "$HERE/site" "$@"

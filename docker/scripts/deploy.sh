#!/usr/bin/env bash
# Build the production roy + roy-web images and roll them out to
# agent.telagon.io. Mirrors the layout used by ../claude-agent/scripts/deploy.sh.
#
# Local arch is arm64 (mac) but the server is x86_64, so we cross-compile via
# buildx and push to ghcr.io. The server's compose file uses `pull_policy: always`
# — `docker compose pull && up -d` picks up the new tag.
#
# Requirements:
#   - `docker buildx` with linux/amd64 support
#   - logged in to ghcr.io (`docker login ghcr.io`)
#   - SSH key trust for $DEPLOY_HOST
#
# Usage:
#   scripts/deploy.sh             # build both, push, remote pull/up
#   scripts/deploy.sh --no-build  # skip image build (push-only / config-only)
#   scripts/deploy.sh --only roy  # build just the rust image
#   scripts/deploy.sh --only web  # build just the SPA

set -euo pipefail

ROY_IMAGE="${ROY_IMAGE:-ghcr.io/strelov1/roy:latest}"
WEB_IMAGE="${WEB_IMAGE:-ghcr.io/strelov1/roy-web:latest}"
PLATFORM="${PLATFORM:-linux/amd64}"
DEPLOY_HOST="${DEPLOY_HOST:-root@204.168.174.129}"
DEPLOY_DIR="${DEPLOY_DIR:-/opt/roy}"
# Prod SPA talks to the gateway through the public hostname; nginx terminates
# TLS and rewrites /ws/ → 127.0.0.1:8788. Override via env if you change paths.
VITE_ROY_WS_URL="${VITE_ROY_WS_URL:-wss://agent.telagon.io/ws}"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REPO_ROOT="$(cd "$ROOT/.." && pwd)"
cd "$REPO_ROOT"

skip_build=0
only=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-build) skip_build=1; shift ;;
    --only) only="${2:-}"; shift 2 ;;
    --only=*) only="${1#*=}"; shift ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

build_roy() {
  echo "==> Building $ROY_IMAGE for $PLATFORM"
  docker buildx build \
    --platform "$PLATFORM" \
    --tag "$ROY_IMAGE" \
    --file docker/Dockerfile.roy \
    --push \
    --provenance=false \
    .
}

build_web() {
  echo "==> Building $WEB_IMAGE for $PLATFORM (VITE_ROY_WS_URL=$VITE_ROY_WS_URL)"
  docker buildx build \
    --platform "$PLATFORM" \
    --tag "$WEB_IMAGE" \
    --file docker/Dockerfile.web \
    --build-arg "VITE_ROY_WS_URL=$VITE_ROY_WS_URL" \
    --push \
    --provenance=false \
    .
}

if [[ "$skip_build" -eq 0 ]]; then
  case "$only" in
    roy) build_roy ;;
    web) build_web ;;
    "")  build_roy; build_web ;;
    *)   echo "unknown --only target: $only (use 'roy' or 'web')" >&2; exit 2 ;;
  esac
else
  echo "==> Skipping build (--no-build)"
fi

echo "==> Rolling out on $DEPLOY_HOST"
# shellcheck disable=SC2087
ssh -o ServerAliveInterval=30 "$DEPLOY_HOST" bash -s <<EOF
set -euo pipefail
cd "$DEPLOY_DIR"
docker compose pull
docker compose up -d
docker image prune -f >/dev/null
echo "---"
docker ps --filter "name=roy-" --format 'table {{.Names}}\t{{.Status}}'
EOF

echo "==> Done. Verify: ssh $DEPLOY_HOST 'curl -fsS http://127.0.0.1:8080/'"

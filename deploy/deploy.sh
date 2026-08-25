#!/usr/bin/env bash
# Green-validate + swap deployment for kiro-rs.
#
# Why not a true blue-green (two live instances behind nginx): the app assumes it is
# the single writer of its data directory. Refresh tokens rotate on use and are written
# back to credentials.json; two instances sharing the directory overwrite each other's
# rotated tokens and knock accounts offline (see ARCHITECTURE.md §8). So the green
# instance runs against an ISOLATED data dir (no credentials) purely to validate the
# build boots and serves, then the real instance is swapped in place. Expected gap:
# ~10-15s (graceful drain is hard-capped at 8s in-app + startup credential load).
#
# Usage (on the server):
#   deploy/deploy.sh <git-ref>          # e.g. deploy/deploy.sh master, or a sha
#   deploy/deploy.sh <git-ref> --skip-green   # skip the green smoke (emergencies only)
#
# Layout it expects:
#   ~/kiro-rs/                deploy dir: docker-compose.yml + data/
#   ~/kiro-src/               git clone used for builds (created on first run)
#   ~/kiro-green/             throwaway green data dir (recreated per run)
set -euo pipefail

REPO_URL="https://github.com/Runix-lab/kiro.rs.git"
DEPLOY_DIR="$HOME/kiro-rs"
SRC_DIR="$HOME/kiro-src"
GREEN_DIR="$HOME/kiro-green"
GREEN_PORT=18080
LIVE_PORT=8080

REF="${1:?usage: deploy.sh <git-ref> [--skip-green]}"
SKIP_GREEN="${2:-}"

log() { echo "[deploy $(date +%H:%M:%S)] $*"; }
die() { echo "[deploy] FATAL: $*" >&2; exit 1; }

# ---------- 1. fetch source at ref ----------
if [ ! -d "$SRC_DIR/.git" ]; then
  log "cloning $REPO_URL -> $SRC_DIR"
  git clone "$REPO_URL" "$SRC_DIR"
fi
git -C "$SRC_DIR" fetch origin --tags
git -C "$SRC_DIR" checkout -q --detach "$(git -C "$SRC_DIR" rev-parse "origin/$REF" 2>/dev/null || git -C "$SRC_DIR" rev-parse "$REF")"
SHA="$(git -C "$SRC_DIR" rev-parse --short HEAD)"
IMAGE="kiro-rs:$SHA"
log "building $IMAGE from $(git -C "$SRC_DIR" log -1 --oneline)"

# ---------- 2. build image ----------
docker build -t "$IMAGE" "$SRC_DIR"

# ---------- 3. green smoke: isolated data dir, no credentials ----------
if [ "$SKIP_GREEN" != "--skip-green" ]; then
  log "green smoke on 127.0.0.1:$GREEN_PORT (isolated data dir, no credentials)"
  rm -rf "$GREEN_DIR" && mkdir -p "$GREEN_DIR/data"
  # Copy runtime config only. credentials.json is deliberately left out so the green
  # instance cannot refresh real tokens (single-writer constraint, ARCHITECTURE.md §8).
  cp "$DEPLOY_DIR/data/config.json" "$GREEN_DIR/data/config.json"
  docker rm -f kiro-rs-green >/dev/null 2>&1 || true
  docker run -d --name kiro-rs-green \
    -p "127.0.0.1:$GREEN_PORT:8990" \
    -v "$GREEN_DIR/data:/app/config" \
    "$IMAGE" >/dev/null

  ok=""
  for i in $(seq 1 30); do
    if curl -fsS -o /dev/null "http://127.0.0.1:$GREEN_PORT/admin" 2>/dev/null; then ok=1; break; fi
    sleep 1
  done
  if [ -z "$ok" ]; then
    docker logs --tail 50 kiro-rs-green || true
    docker rm -f kiro-rs-green >/dev/null 2>&1 || true
    die "green instance never served /admin within 30s — aborting, live untouched"
  fi
  # /v1/models must gate on auth (401 without key) — proves the API stack is wired.
  code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$GREEN_PORT/v1/models")
  [ "$code" = "401" ] || { docker rm -f kiro-rs-green >/dev/null 2>&1 || true; die "green /v1/models expected 401 got $code"; }
  docker rm -f kiro-rs-green >/dev/null 2>&1 || true
  rm -rf "$GREEN_DIR"
  log "green smoke passed"
fi

# ---------- 4. swap live instance ----------
COMPOSE="$DEPLOY_DIR/docker-compose.yml"
PREV_IMAGE="$(grep -E '^\s*image:' "$COMPOSE" | awk '{print $2}')"
cp "$COMPOSE" "$COMPOSE.bak-$(date +%Y%m%d-%H%M%S)"
sed -i -E "s|^(\s*image:)\s*\S+|\1 $IMAGE|" "$COMPOSE"
log "swapping $PREV_IMAGE -> $IMAGE (expect ~10-15s gap)"
SWAP_T0=$(date +%s)
(cd "$DEPLOY_DIR" && docker compose up -d)

# ---------- 5. health check + rollback ----------
ok=""
for i in $(seq 1 45); do
  if curl -fsS -o /dev/null "http://127.0.0.1:$LIVE_PORT/admin" 2>/dev/null; then ok=1; break; fi
  sleep 1
done
SWAP_T1=$(date +%s)
if [ -z "$ok" ]; then
  log "live instance unhealthy after swap — ROLLING BACK to $PREV_IMAGE"
  docker logs --tail 50 kiro-rs || true
  sed -i -E "s|^(\s*image:)\s*\S+|\1 $PREV_IMAGE|" "$COMPOSE"
  (cd "$DEPLOY_DIR" && docker compose up -d)
  for i in $(seq 1 45); do
    curl -fsS -o /dev/null "http://127.0.0.1:$LIVE_PORT/admin" 2>/dev/null && break
    sleep 1
  done
  die "deploy failed; rolled back to $PREV_IMAGE"
fi
log "live healthy on $IMAGE (swap window ${SWAP_T1}-${SWAP_T0}s wall: $((SWAP_T1-SWAP_T0))s)"
log "done. previous image kept: $PREV_IMAGE (rollback: edit compose + docker compose up -d)"

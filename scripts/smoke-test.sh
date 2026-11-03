#!/usr/bin/env bash
# Runs a built dinghy image against the local Docker daemon and checks the
# basics end to end: health, assets, sign-in, a compose up/down, memory.
#
#   scripts/smoke-test.sh [image]          (default: dinghy:dev)
#
# SMOKE_DIR overrides the scratch directory (the Docker daemon must be able to
# bind-mount it) and SMOKE_PORT the host port.
set -euo pipefail

IMAGE="${1:-dinghy:dev}"
PORT="${SMOKE_PORT:-5099}"
BASE="http://127.0.0.1:${PORT}"
NAME="dinghy-smoke-$$"
STACK="smoke-$$"
DIR="${SMOKE_DIR:-$(mktemp -d)}"
PASSWORD="smoke-$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n')"

fail() { echo "FAIL: $*" >&2; docker logs "$NAME" 2>&1 | tail -20 >&2 || true; exit 1; }
cleanup() {
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  docker compose -p "$STACK" down --timeout 1 >/dev/null 2>&1 || true
  [ -z "${SMOKE_DIR:-}" ] && rm -rf "$DIR"
}
trap cleanup EXIT

mkdir -p "$DIR/stacks/$STACK"
cat > "$DIR/stacks/$STACK/compose.yaml" <<'EOF'
services:
  app:
    image: alpine:3.22
    command: ["sh", "-c", "echo ready; exec sleep 3600"]
    stop_grace_period: 1s
EOF
chmod -R a+rwX "$DIR/stacks"

HASH="$(printf '%s' "$PASSWORD" | docker run --rm -i "$IMAGE" hash-password)"
SOCK_GID="$(docker run --rm -v /var/run/docker.sock:/s alpine:3.22 stat -c %g /s)"

started=$(date +%s%N)
docker run -d --name "$NAME" \
  --read-only --tmpfs /tmp --cap-drop ALL --security-opt no-new-privileges \
  --memory 64m --group-add "$SOCK_GID" \
  -v /var/run/docker.sock:/var/run/docker.sock \
  --mount "type=bind,source=$DIR/stacks,target=/opt/stacks" \
  -e DINGHY_PASSWORD_HASH="$HASH" \
  -e DINGHY_INSECURE_NO_ACCESS=1 \
  -p "127.0.0.1:${PORT}:5002" \
  "$IMAGE" >/dev/null

for _ in $(seq 1 100); do
  curl -sf "$BASE/healthz" >/dev/null && break
  sleep 0.1
done
curl -sf "$BASE/healthz" >/dev/null || fail "/healthz never answered"
echo "ok   healthy after $(( ($(date +%s%N) - started) / 1000000 )) ms"

docker exec "$NAME" /usr/local/bin/dinghy healthcheck || fail "healthcheck subcommand"
echo "ok   healthcheck subcommand"

# Capture before grepping: with pipefail, `curl | grep -q` fails when grep
# closes the pipe early.
index="$(curl -sf "$BASE/")"
[[ "$index" == *'<div id="app">'* ]] || fail "index.html not served"
js="$(grep -o 'assets/index-[^"]*\.js' <<<"$index" | head -1)"
headers="$(curl -sf -o "$DIR/asset" -D - -H 'Accept-Encoding: br, gzip' "$BASE/$js")"
grep -qi '^content-encoding: br' <<<"$headers" || fail "assets not served precompressed"
echo "ok   frontend served, brotli assets"

code=$(curl -s -o "$DIR/out" -w '%{http_code}' "$BASE/api/stacks")
[ "$code" = 401 ] || fail "/api/stacks without a session returned $code"
login="$(curl -s -i -H 'Content-Type: application/json' -d "{\"password\":\"$PASSWORD\"}" "$BASE/api/login")"
cookie="$(printf '%s' "$login" | grep -i '^set-cookie:' | sed 's/^[^:]*: //; s/;.*//')"
csrf="$(printf '%s' "$login" | tail -1 | sed -n 's/.*"csrf":"\([^"]*\)".*/\1/p')"
[ -n "$cookie" ] && [ -n "$csrf" ] || fail "sign-in failed"
echo "ok   sign-in"

run_action() {
  local op
  op="$(curl -sf -b "$cookie" -H "X-Dinghy-CSRF: $csrf" -X POST "$BASE/api/stacks/$STACK/$1" \
    | sed -n 's/.*"op":"\([^"]*\)".*/\1/p')"
  [ -n "$op" ] || fail "could not start $1"
  local out
  out="$(curl -sf -N --max-time 120 -b "$cookie" "$BASE/api/ops/$op/stream")"
  grep -A1 '^event: done' <<<"$out" | grep -qx 'data: 0' \
    || { printf '%s\n' "$out" >&2; fail "compose $1 did not exit 0"; }
}

run_action up
status="$(curl -sf -b "$cookie" "$BASE/api/stacks/$STACK" | sed -n 's/^{"name":"[^"]*","status":"\([a-z]*\)".*/\1/p')"
[ "$status" = running ] || fail "stack status after up is '$status'"
echo "ok   compose up, stack running"

mem="$(docker stats --no-stream --format '{{.MemUsage}}' "$NAME" | cut -d/ -f1 | xargs)"
echo "ok   memory in use: $mem"

run_action down
echo "ok   compose down"
echo "PASS"

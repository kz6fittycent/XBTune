#!/usr/bin/env bash
# Run the remote-tuning relay Worker locally (wrangler dev, no Cloudflare
# login) and run xbtune-remote's relay tests against it, so the Worker
# behaves like crates/xbtune-remote/src/local_relay.rs.
#   tools/test_remote_relay.sh            # from the repo root
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
port="${PORT:-8792}"
state="$(mktemp -d)"
log="$state/wrangler.log"
export WRANGLER_SEND_METRICS=false
# Own process group, so the cleanup stops wrangler's workerd child too.
setsid bash -c 'cd "$1/relay/remote" && exec npx --yes wrangler@4 dev --port "$2" --ip 127.0.0.1 --persist-to "$3/do"' \
  _ "$root" "$port" "$state" >"$log" 2>&1 &
pid=$!
trap 'kill -- -$pid 2>/dev/null; sleep 1; rm -rf "$state"' EXIT
for _ in $(seq 1 90); do
  grep -q "Ready on" "$log" && break
  kill -0 $pid 2>/dev/null || { cat "$log"; exit 1; }
  sleep 1
done
grep -q "Ready on" "$log" || { cat "$log"; echo "wrangler dev didn't start"; exit 1; }
cd "$root"
XBTUNE_TEST_RELAY="ws://127.0.0.1:$port/v1" cargo test -p xbtune-remote --test external_relay --locked

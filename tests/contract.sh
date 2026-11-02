#!/bin/sh
# Contract test: the real Pi client against the REAL collector.
#
# Builds and starts the collector from the sibling project
# (../mobile_vehicle_counter_server) on a temporary database, then starts the Pi
# client and drives it like a camera would. Checks what README.md promises:
#
#   1. a good event is delivered, and the collector stores it once
#   2. an event for an unregistered camera stays 'pending' (422), and is delivered
#      once the camera is registered, with the same bytes
#   3. an event with a bad date is rejected by the collector (400) -> 'failed'
#   4. unparseable camera JSON is acknowledged, kept as 'failed', never sent
#   5. re-sending an already-delivered event is a harmless duplicate
#
# Usage:   tests/contract.sh            (from anywhere)
# Needs:   cargo, curl, sqlite3. No network access is used (loopback only).
# Env:     SERVER_DIR  path of the collector project (default ../mobile_vehicle_counter_server)
#          COLLECTOR_PORT / PI_PORT  (defaults 3901 / 3902)

set -u

PI_DIR=$(cd "$(dirname "$0")/.." && pwd)
SERVER_DIR=${SERVER_DIR:-"$PI_DIR/../mobile_vehicle_counter_server"}
COLLECTOR_PORT=${COLLECTOR_PORT:-3901}
PI_PORT=${PI_PORT:-3902}

for tool in cargo curl sqlite3; do
    command -v "$tool" >/dev/null 2>&1 || { echo "missing tool: $tool" >&2; exit 2; }
done
[ -f "$SERVER_DIR/Cargo.toml" ] || { echo "collector project not found: $SERVER_DIR" >&2; exit 2; }

WORK=$(mktemp -d)
COLLECTOR_PID=""
PI_PID=""
cleanup() {
    [ -n "$PI_PID" ] && kill "$PI_PID" 2>/dev/null
    [ -n "$COLLECTOR_PID" ] && kill "$COLLECTOR_PID" 2>/dev/null
    wait 2>/dev/null
    rm -rf "$WORK"
}
trap cleanup EXIT INT TERM

PASS=0
FAIL=0
check() { # check NAME EXPECTED ACTUAL
    if [ "$2" = "$3" ]; then
        PASS=$((PASS + 1)); printf 'PASS  %s\n' "$1"
    else
        FAIL=$((FAIL + 1)); printf 'FAIL  %s: expected [%s], got [%s]\n' "$1" "$2" "$3"
    fi
}

# wait_for DESCRIPTION SECONDS COMMAND...: poll until the command succeeds.
wait_for() {
    what=$1; limit=$2; shift 2
    i=0
    while [ "$i" -lt $((limit * 10)) ]; do
        "$@" >/dev/null 2>&1 && return 0
        sleep 0.1; i=$((i + 1))
    done
    echo "timed out waiting for: $what" >&2
    return 1
}

echo "== building =="
(cd "$SERVER_DIR" && cargo build --offline --bin run_data_collector) || { echo "collector build failed" >&2; exit 2; }
(cd "$PI_DIR" && cargo build --offline --bin run_pi_client) || { echo "pi build failed" >&2; exit 2; }
COLLECTOR_BIN="$SERVER_DIR/target/debug/run_data_collector"
PI_BIN="$PI_DIR/target/debug/run_pi_client"

SERVER_DB="$WORK/collector/server.db"
PI_DB="$WORK/pi/pi_events.db"
mkdir -p "$WORK/collector" "$WORK/pi"

echo "== starting the collector on 127.0.0.1:$COLLECTOR_PORT =="
(
    cd "$WORK/collector" && \
    DATABASE_URL="sqlite://$SERVER_DB?mode=rw" IMAGE_DIR="$WORK/collector/images" \
    COLLECTOR_ADDR="127.0.0.1:$COLLECTOR_PORT" JWT_SECRET=unused \
    exec "$COLLECTOR_BIN"
) >"$WORK/collector.log" 2>&1 &
COLLECTOR_PID=$!
wait_for "collector database" 30 test -s "$SERVER_DB" || { cat "$WORK/collector.log"; exit 2; }
wait_for "collector port" 30 sh -c "curl -s -o /dev/null http://127.0.0.1:$COLLECTOR_PORT/ " || { cat "$WORK/collector.log"; exit 2; }

# The collector rejects cameras it does not know, so register one.
sqlite3 "$SERVER_DB" "
  INSERT INTO location(id,name,description) VALUES('L1','Loc','d');
  INSERT INTO sublocation(id,name,description,location_id) VALUES('S1','Sub',NULL,'L1');
  INSERT INTO camera(id,name,description,sublocation_id,orientation) VALUES('CAM1','Cam','d','S1','Inbound');"

echo "== starting the Pi client on 127.0.0.1:$PI_PORT =="
(
    cd "$WORK/pi" && \
    COLLECTOR_URL="http://127.0.0.1:$COLLECTOR_PORT/api/alpr/events" COLLECTOR_TOKEN=test-token \
    PI_DB_PATH="$PI_DB" PI_LISTEN_ADDR="127.0.0.1:$PI_PORT" \
    BACKOFF_CAP_SECS=4 REQUEST_TIMEOUT_SECS=20 \
    exec "$PI_BIN"
) >"$WORK/pi.log" 2>&1 &
PI_PID=$!
wait_for "pi database" 30 test -s "$PI_DB" || { cat "$WORK/pi.log"; exit 2; }
wait_for "pi port" 30 sh -c "curl -s -o /dev/null http://127.0.0.1:$PI_PORT/" || { cat "$WORK/pi.log"; exit 2; }

CAMERA_URL="http://127.0.0.1:$PI_PORT/api/alpr/events"
NOW=$(date -u +%Y-%m-%dT%H:%M:%SZ)
IMG='/9j/4AAQ'

camera_post() { # camera_post BODY -> prints the HTTP status
    curl -s -o /dev/null -w '%{http_code}' --max-time 20 -X POST "$CAMERA_URL" \
        -H 'Content-Type: application/json' --data-binary "$1"
}
pi_query() { sqlite3 "$PI_DB" "$1"; }
server_query() { sqlite3 "$SERVER_DB" "$1"; }

echo "== scenarios =="

# 1. a good event
check "camera gets 200 for a good event" 200 \
    "$(camera_post "{\"camid\":\"CAM1\",\"date\":\"$NOW\",\"plate\":\"GOOD0001\",\"image\":\"$IMG\",\"plate_image\":\"$IMG\"}")"
wait_for "good event delivered" 20 sh -c "[ \"\$(sqlite3 '$PI_DB' \"select status from events where json_extract(cast(body as text),'\$.plate')='GOOD0001'\")\" = delivered ]"
check "good event is 'delivered' on the Pi" delivered \
    "$(pi_query "select status from events where json_extract(cast(body as text),'\$.plate')='GOOD0001'")"
check "collector stored the good event once" 1 \
    "$(server_query "select count(*) from alpr_events where plate='GOOD0001'")"
GOOD_ID=$(pi_query "select event_id from events where json_extract(cast(body as text),'\$.plate')='GOOD0001'")
pi_query "select writefile('$WORK/good_body.bin', body) from events where event_id='$GOOD_ID'" >/dev/null
PI_HASH=$(sha256sum "$WORK/good_body.bin" | cut -d' ' -f1)
check "the collector's stored hash equals the hash of the Pi's stored bytes" "$PI_HASH" \
    "$(server_query "select content_hash from alpr_events where id='$GOOD_ID'")"

# 2. unregistered camera: stays pending, then succeeds once registered
check "camera gets 200 for an unregistered camera" 200 \
    "$(camera_post "{\"camid\":\"CAM9\",\"date\":\"$NOW\",\"plate\":\"LATE0001\",\"image\":\"$IMG\",\"plate_image\":\"$IMG\"}")"
wait_for "422 recorded" 20 sh -c "[ \"\$(sqlite3 '$PI_DB' \"select attempts from events where json_extract(cast(body as text),'\$.plate')='LATE0001'\")\" -ge 1 ]"
check "unregistered camera event stays 'pending'" pending \
    "$(pi_query "select status from events where json_extract(cast(body as text),'\$.plate')='LATE0001'")"
LATE_BODY_BEFORE=$(pi_query "select hex(body) from events where json_extract(cast(body as text),'\$.plate')='LATE0001'")
sqlite3 "$SERVER_DB" "INSERT INTO camera(id,name,description,sublocation_id,orientation) VALUES('CAM9','Cam9','d','S1','Outbound');"
wait_for "late event delivered" 30 sh -c "[ \"\$(sqlite3 '$PI_DB' \"select status from events where json_extract(cast(body as text),'\$.plate')='LATE0001'\")\" = delivered ]"
check "event is delivered once the camera is registered" delivered \
    "$(pi_query "select status from events where json_extract(cast(body as text),'\$.plate')='LATE0001'")"
check "its stored bytes never changed" "$LATE_BODY_BEFORE" \
    "$(pi_query "select hex(body) from events where json_extract(cast(body as text),'\$.plate')='LATE0001'")"
check "collector stored the late event once" 1 \
    "$(server_query "select count(*) from alpr_events where plate='LATE0001'")"

# 3. bad date: the collector answers 400 -> failed
check "camera gets 200 for an event with a bad date" 200 \
    "$(camera_post "{\"camid\":\"CAM1\",\"date\":\"yesterday\",\"plate\":\"BADDATE1\",\"image\":\"$IMG\",\"plate_image\":\"$IMG\"}")"
wait_for "bad date failed" 20 sh -c "[ \"\$(sqlite3 '$PI_DB' \"select status from events where json_extract(cast(body as text),'\$.plate')='BADDATE1'\")\" = failed ]"
check "bad-date event is 'failed' on the Pi" failed \
    "$(pi_query "select status from events where json_extract(cast(body as text),'\$.plate')='BADDATE1'")"
check "bad-date event was not stored by the collector" 0 \
    "$(server_query "select count(*) from alpr_events where plate='BADDATE1'")"

# 4. unparseable camera JSON
check "camera gets 200 for unparseable JSON" 200 "$(camera_post '{not json')"
check "unparseable post is kept as a 'failed' row with its raw bytes" 1 \
    "$(pi_query "select count(*) from events where status='failed' and cast(body as text)='{not json' and attempts=0")"

# 5. duplicate: put a delivered event back to pending and let the Pi resend it
pi_query "update events set status='pending', next_attempt_at=0 where event_id='$GOOD_ID'"
wait_for "duplicate resend" 20 sh -c "[ \"\$(sqlite3 '$PI_DB' \"select status from events where event_id='$GOOD_ID'\")\" = delivered ]"
check "a re-sent event is delivered again" delivered "$(pi_query "select status from events where event_id='$GOOD_ID'")"
check "the collector still holds it exactly once" 1 \
    "$(server_query "select count(*) from alpr_events where id='$GOOD_ID'")"

echo
echo "$PASS passed, $FAIL failed"
if [ "$FAIL" -ne 0 ]; then
    echo "---- pi log ----";        tail -n 20 "$WORK/pi.log"
    echo "---- collector log ----"; tail -n 20 "$WORK/collector.log"
    exit 1
fi

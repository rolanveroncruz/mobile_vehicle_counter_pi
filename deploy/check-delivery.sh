#!/bin/sh
# End-to-end check, run ON THE PI: camera -> Pi client -> collector server.
#
# Posts ONE camera-style event to the running Pi client, then waits for the Pi's
# database to say 'delivered'. 'delivered' only happens after the collector has
# stored the event and returned a receipt that matches what the Pi sent, so it
# proves the whole path: Pi client, network, HTTPS, nginx token check, collector.
#
# Usage:   deploy/check-delivery.sh [CAMID] [PI_URL]
#
#   CAMID   default CAM1. Must exist in the server's camera table, otherwise the
#           event stays 'pending' (collector answers 422). Quote it if it has
#           spaces, e.g. "InSight LPR_Front".
#   PI_URL  default http://127.0.0.1:3000/api/alpr/events (the Pi client directly).
#           Use http://127.0.0.1/api/alpr/events to test through the Pi's nginx.
#
# Env:     PI_DB_PATH      the Pi's database (default: read from .env, else
#                          pi_data/pi_events.db)
#          WAIT_SECS       how long to wait for delivery (default 30)
#
# Needs:   curl, sqlite3. The Pi client must be running.
#
# The event is REALLY stored on the collector server (plate "CHKhhmmss..."), so
# delete it there afterwards. It stays in the Pi's database too (rows are never
# deleted).

set -u

PROJECT_DIR=$(cd "$(dirname "$0")/.." && pwd)
cd "$PROJECT_DIR" || exit 2

CAMID="${1:-CAM1}"
URL="${2:-http://127.0.0.1:3000/api/alpr/events}"
WAIT_SECS="${WAIT_SECS:-30}"

for tool in curl sqlite3; do
    command -v "$tool" >/dev/null 2>&1 || { echo "missing tool: $tool" >&2; exit 2; }
done

# Where is the Pi's database? Environment, then .env, then the default.
DB="${PI_DB_PATH:-}"
if [ -z "$DB" ] && [ -f .env ]; then
    DB=$(sed -n 's/^PI_DB_PATH=//p' .env | tail -n 1 | tr -d '"' | tr -d "'")
fi
DB="${DB:-pi_data/pi_events.db}"
if [ ! -r "$DB" ]; then
    echo "cannot read the Pi database: $DB" >&2
    echo "  - is the Pi client set up and has it run once?" >&2
    echo "  - set PI_DB_PATH, or run as the service user: sudo -u mvc $0" >&2
    exit 2
fi

PLATE="CHK$(date +%H%M%S)$$"
NOW=$(date -u +%Y-%m-%dT%H:%M:%SZ)
IMG='/9j/4AAQ'   # start of a JPEG in base64; the collector only base64-decodes it
BODY=$(printf '{"camid":"%s","date":"%s","plate":"%s","image":"%s","plate_image":"%s"}' \
    "$CAMID" "$NOW" "$PLATE" "$IMG" "$IMG")

echo "POST $URL"
echo "  camid $CAMID, plate $PLATE"

STATUS=$(curl -s -o /dev/null -w '%{http_code}' --max-time 30 \
    -X POST "$URL" -H "Content-Type: application/json" --data-binary "$BODY")
echo "  Pi client answered HTTP $STATUS"

if [ "$STATUS" != "200" ]; then
    echo
    echo "FAILED: the Pi client did not accept the event."
    case "$STATUS" in
        000) echo "Could not connect. Is run_pi_client running? Is the URL right? ($URL)" ;;
        502|504) echo "nginx is up but run_pi_client is not answering. systemctl status mobile-vehicle-pi" ;;
        413) echo "Body too large for nginx or MAX_CAMERA_BODY_BYTES. Check client_max_body_size." ;;
        404) echo "Wrong path. It must be /api/alpr/events." ;;
        500) echo "The Pi could not write its database (disk full? permissions?). See the Pi log." ;;
        *)   echo "See: journalctl -u mobile-vehicle-pi -n 50" ;;
    esac
    exit 1
fi

query() {
    sqlite3 -readonly "$DB" \
        "select status || '|' || attempts || '|' || coalesce(last_error,'') from events
         where json_extract(cast(body as text),'\$.plate') = '$PLATE'
         order by created_at desc limit 1;" 2>/dev/null
}

echo "  waiting up to ${WAIT_SECS}s for delivery to the collector..."
i=0
RESULT=""
while [ "$i" -lt $((WAIT_SECS * 2)) ]; do
    RESULT=$(query)
    case "$RESULT" in
        delivered\|*|failed\|*) break ;;
    esac
    sleep 0.5
    i=$((i + 1))
done

STATE=${RESULT%%|*}
REST=${RESULT#*|}
ATTEMPTS=${REST%%|*}
ERROR=${REST#*|}

echo
case "$STATE" in
    delivered)
        echo "OK: delivered after $ATTEMPTS attempt(s). The collector stored the event and the receipt matched."
        echo "Delete plate $PLATE from the server's database when you are done."
        exit 0 ;;
    failed)
        echo "FAILED: the collector refused the event for good: $ERROR"
        case "$ERROR" in
            *400*) echo "  Bad request. Check the date format (RFC 3339 with an offset)." ;;
            *409*) echo "  event_id conflict. Unexpected for a new event: see the Pi log." ;;
            *) echo "  See the Pi log: journalctl -u mobile-vehicle-pi -n 50" ;;
        esac
        exit 1 ;;
    pending)
        echo "NOT DELIVERED YET: still 'pending' after ${WAIT_SECS}s ($ATTEMPTS attempt(s))."
        echo "  last error: $ERROR"
        case "$ERROR" in
            *401*) echo "  Token rejected. COLLECTOR_TOKEN must equal the one in the server's /etc/nginx/collector-token.conf." ;;
            *422*) echo "  Camera '$CAMID' is not in the server's camera table. Register it (the Pi retries slowly) or pass another CAMID." ;;
            *"request failed"*) echo "  Could not reach the collector. Check COLLECTOR_URL, DNS, the network and the server's port 443." ;;
            *5[0-9][0-9]*) echo "  Server or proxy error. Check the collector: systemctl status on the server." ;;
        esac
        echo "  The event stays safely queued and will keep being retried."
        exit 1 ;;
    *)
        echo "FAILED: the event is not in the Pi database ($DB)."
        echo "  Is the Pi client using a different database? Set PI_DB_PATH to the one it uses."
        exit 1 ;;
esac

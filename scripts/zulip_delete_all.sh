#!/bin/bash
# Full Zulip cleanup: delete all bot messages from DMs and all streams.
# Run before QA to provide a clean slate.
# Usage: ./zulip_delete_all.sh

INTEGRATION_EMAIL="${ZULIP_BOT_EMAIL:?set ZULIP_BOT_EMAIL}"
INTEGRATION_KEY="${ZULIP_BOT_KEY:?set ZULIP_BOT_KEY}"
TURBOJET_EMAIL="${TURBOJET_BOT_EMAIL:?set TURBOJET_BOT_EMAIL}"
ZULIP_URL="${ZULIP_URL:?set ZULIP_URL (for example https://zulip.example.com)}"

set -a
source config/.env
set +a

STREAMS=("test-project" "turbojet" "general")
DM_NARROW="%5B%7B%22operator%22%3A%22pm-with%22%2C%22operand%22%3A%22${TURBOJET_EMAIL}%22%7D%5D"

delete_bot_messages() {
  local BOT_EMAIL="$1"
  local BOT_KEY="$2"
  local NARROW="$3"
  local LABEL="$4"

  MESSAGES=$(curl -s -u "${BOT_EMAIL}:${BOT_KEY}" \
    "${ZULIP_URL}/api/v1/messages?anchor=1000000000&num_before=1000&num_after=0&narrow=${NARROW}")

  OWN_IDS=$(echo "$MESSAGES" | jq -r '.messages[] | select(.sender_email == "'${BOT_EMAIL}'") | .id' 2>/dev/null)

  if [ -z "$OWN_IDS" ]; then
    echo "  No ${LABEL} messages"
    return
  fi

  COUNT=$(echo "$OWN_IDS" | wc -l | tr -d ' ')
  echo "  Deleting ${COUNT} ${LABEL} messages..."
  echo "$OWN_IDS" | while read -r MSG_ID; do
    [ -z "$MSG_ID" ] && continue
    curl -s -X DELETE -u "${BOT_EMAIL}:${BOT_KEY}" \
      "${ZULIP_URL}/api/v1/messages/${MSG_ID}" > /dev/null 2>&1
  done
}

echo "=== DM Cleanup ==="
delete_bot_messages "$INTEGRATION_EMAIL" "$INTEGRATION_KEY" "$DM_NARROW" "integration-bot DM"
delete_bot_messages "$TURBOJET_EMAIL" "$ZULIP_API_KEY" "$DM_NARROW" "TurboJet-bot DM"

echo ""
echo "=== Stream Cleanup ==="
for STREAM in "${STREAMS[@]}"; do
  echo "--- $STREAM ---"
  STREAM_NARROW="%5B%7B%22operator%22%3A%22stream%22%2C%22operand%22%3A%22${STREAM}%22%7D%5D"
  delete_bot_messages "$INTEGRATION_EMAIL" "$INTEGRATION_KEY" "$STREAM_NARROW" "integration-bot"
  delete_bot_messages "$TURBOJET_EMAIL" "$ZULIP_API_KEY" "$STREAM_NARROW" "TurboJet-bot"
done

echo ""
echo "Cleanup complete"

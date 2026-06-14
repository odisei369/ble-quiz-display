#!/usr/bin/env bash
set -euo pipefail

# Run the BLE bridge pointed at the deployed quiz server.
# Override QUIZ_URL for local development:
#   QUIZ_URL=http://localhost:3000 ./run-demo.sh
export QUIZ_URL="${QUIZ_URL:-https://quiz.nidobit.com}"

BRIDGE="$(dirname "$0")/bridge/target/release/bridge"
if [ ! -x "$BRIDGE" ]; then
  echo "Bridge binary missing — building release..."
  (cd "$(dirname "$0")/bridge" && cargo build --release)
fi

exec "$BRIDGE"

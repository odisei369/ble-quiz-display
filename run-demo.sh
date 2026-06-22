#!/usr/bin/env bash
set -euo pipefail

# Run the BLE bridge pointed at the deployed quiz server.
# Override QUIZ_URL for local development:
#   QUIZ_URL=http://localhost:3000 ./run-demo.sh
export QUIZ_URL="${QUIZ_URL:-https://quiz.nidobit.com}"

# bridge/.cargo/config.toml pins the host target, so the binary lands under
# target/aarch64-apple-darwin/, not the default target/release/.
BRIDGE="$(dirname "$0")/bridge/target/aarch64-apple-darwin/release/bridge"
(cd "$(dirname "$0")/bridge" && cargo build --release)

exec "$BRIDGE"

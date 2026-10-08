#!/bin/bash
# End-to-end exercise of the console commands against a real node on this machine.
#
# Everything runs under a `--server` instance, which is the whole point of the instance name: it
# touches no production data, keeps its own credential, settings, log and lock, and can be
# deleted afterwards.
set -u

cd "$(dirname "$0")/.." || exit 1
BIN="$PWD/target/debug/zyris"
SERVER="wss://127.0.0.1:1/ws"
DATA="$HOME/.local/share/zyris-dev-wss---127-0-0-1-1-ws"

run() {
  echo
  echo "### $*"
  "$@"
  echo "[exit $?]"
}

rm -rf "$DATA"

run "$BIN" --version
run "$BIN" --help

echo
echo "### nothing is running yet"
run "$BIN" status --server "$SERVER"
run "$BIN" down --server "$SERVER"

echo
echo "### settings, without a window"
run "$BIN" config list --server "$SERVER"
run "$BIN" config set voice.listen true --server "$SERVER"
run "$BIN" config set voice.session s_01H --server "$SERVER"
run "$BIN" config set voice.volume 1.5 --server "$SERVER"
run "$BIN" config set voice.device default --server "$SERVER"
run "$BIN" config get voice.listen --server "$SERVER"
run "$BIN" config set voice.session unset --server "$SERVER"
run "$BIN" config get voice.session --server "$SERVER"
run "$BIN" config set voice.listen maybe --server "$SERVER"
run "$BIN" config set voice.nonsense true --server "$SERVER"
echo "### the file it wrote"
cat "$DATA/voice.json"

echo
echo "### MCP servers, without a window"
run "$BIN" mcp list --server "$SERVER"
# The flags go before the subcommand here: everything after the command is the server's own
run "$BIN" --server "$SERVER" mcp add notes mcp-notes --dir /tmp
run "$BIN" mcp list --server "$SERVER"
run "$BIN" mcp disable notes --server "$SERVER"
run "$BIN" mcp remove notes --server "$SERVER"
run "$BIN" mcp remove notes --server "$SERVER"
echo "### the file it wrote"
cat "$DATA/mcp-servers.json" 2>&1

echo
echo "### starting a node in the background"
run "$BIN" up --server "$SERVER" --headless
sleep 3
run "$BIN" status --server "$SERVER"
echo "### what the node said"
head -25 "$DATA/node.log"
echo "### the state file"
cat "$DATA/node-state.json"

echo
echo "### a second up while one is running"
run "$BIN" up --server "$SERVER" --headless

echo
echo "### stopping it"
run "$BIN" down --server "$SERVER"
run "$BIN" status --server "$SERVER"

echo
echo "### login, against a server that is not there"
timeout 30 "$BIN" login --server "$SERVER"
echo "[exit $?]"

echo
echo "### done"

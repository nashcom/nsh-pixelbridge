#!/bin/sh
# Examples against a running nsh-pixelbridge.
#
# The port comes from ../.env (PIXELBRIDGE_PORT, the same file compose.yml
# uses), so the script talks to the container you started with compose.
#
#   ./examples/curl-example.sh                       # converts examples/nashcom.svg
#   ./examples/curl-example.sh path/to/other.svg     # your own SVG
#   BASE=https://pixelbridge.example.com ./examples/curl-example.sh   # another server
set -eu

DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"

if [ -f "$DIR/../.env" ]; then
    set -a
    . "$DIR/../.env"
    set +a
fi

BASE="${BASE:-http://localhost:${PIXELBRIDGE_PORT:-8080}}"

if [ $# -ge 1 ]; then
    SVG="$1"
else
    SVG="$DIR/nashcom.svg"
fi
NAME="$(basename "$SVG" .svg)"

echo "server: $BASE"
echo "input : $SVG"

echo "== health"
curl -fsS "$BASE/health"; echo

WIDTH=256
echo "== SVG -> PNG, 512 pixels wide (raw bytes in, raw bytes out) -> $NAME.png"
curl -fsS -X POST "$BASE/v1/image/convert?format=png&width=$WIDTH" \
    -H 'Content-Type: image/svg+xml' --data-binary @"$SVG" -o "$NAME.png"
file "$NAME.png" 2>/dev/null || ls -l "$NAME.png"

echo "== SVG -> JPEG (transparent areas become white) -> $NAME.jpg"
curl -fsS -X POST "$BASE/v1/image/convert?format=jpeg&width=$WIDTH&quality=100" \
    -H 'Content-Type: image/svg+xml' --data-binary @"$SVG" -o "$NAME.jpg"
ls -l "$NAME.jpg"

echo "== JSON endpoint (what the MCP bridge uses): SVG text in, base64 out"
curl -fsS -X POST "$BASE/v1/image/convert-json?format=png&width=40" -H 'Content-Type: application/json' \
    -d '{"svg":"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"20\" height=\"10\"><rect width=\"20\" height=\"10\" fill=\"red\"/></svg>"}'
echo

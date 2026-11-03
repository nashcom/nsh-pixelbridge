#!/bin/sh
# End-to-end test of the built container image: starts it the way production
# does (read-only root, non-root user, no capabilities), then talks HTTP to it.
#
# Usage:
#   ./build.sh                          # build the image first
#   tests/integration_test.sh           # test nsh-pixelbridge:latest
#   tests/integration_test.sh my/image:tag
#   PORT=18090 tests/integration_test.sh
#
# Needs: docker, curl. Uses a high port on 127.0.0.1 and refuses to run if
# something already answers there.
set -u

IMAGE="${1:-nsh-pixelbridge:latest}"
PORT="${PORT:-18089}"
NAME="pixelbridge-it-$$"
BASE="http://127.0.0.1:$PORT"
DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
TMP="$(mktemp -d)"
FAILED=0

cleanup()
{
    docker rm -f "$NAME" >/dev/null 2>&1
    rm -rf "$TMP"
}
trap cleanup EXIT

check()
{
    # check "description" expected actual
    if [ "$2" = "$3" ]; then
        echo "  ok    $1"
    else
        echo "  FAIL  $1 (expected '$2', got '$3')"
        FAILED=1
    fi
}

if curl -s -o /dev/null --max-time 1 "$BASE/"; then
    echo "port $PORT already answers HTTP - pick another with PORT=..." >&2
    exit 2
fi

echo "== starting $IMAGE"
docker run -d --name "$NAME" --read-only --user 1000:1000 \
    --cap-drop ALL --security-opt no-new-privileges:true \
    --memory 512m --pids-limit 64 \
    -p "127.0.0.1:$PORT:8080" "$IMAGE" >/dev/null || exit 2

i=0
until curl -fs -o /dev/null "$BASE/health"; do
    i=$((i + 1))
    if [ "$i" -gt 20 ]; then
        echo "service did not come up:" >&2
        docker logs "$NAME" >&2
        exit 2
    fi
    sleep 0.5
done

echo "== checks"
check "health status" "ok" "$(curl -fs "$BASE/health" | sed -n 's/.*"status":"\([a-z]*\)".*/\1/p')"
check "-health-check inside the container" "0" "$(docker exec "$NAME" /nsh-pixelbridge -health-check >/dev/null 2>&1; echo $?)"
check "runs as uid 1000" "1000:1000" "$(docker inspect -f '{{.Config.User}}' "$NAME")"
check "/docs (Swagger UI page)" "200" "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/docs")"
check "openapi title" "NashCom PixelBridge API" "$(curl -fs "$BASE/openapi.json" | sed -n 's/.*"title":"\([^"]*\)".*/\1/p')"

curl -fs -X POST "$BASE/v1/image/convert?format=png&width=300" -H 'Content-Type: image/svg+xml' \
    --data-binary @"$DIR/../testdata/simple.svg" -o "$TMP/out.png"
check "SVG -> PNG signature" "89504e470d0a1a0a" "$(head -c 8 "$TMP/out.png" | od -An -tx1 | tr -d ' \n')"

curl -fs -X POST "$BASE/v1/image/convert?format=jpeg" -H 'Content-Type: image/svg+xml' \
    --data-binary @"$DIR/../testdata/transparent.svg" -o "$TMP/out.jpg"
check "SVG -> JPEG signature" "ffd8ff" "$(head -c 3 "$TMP/out.jpg" | od -An -tx1 | tr -d ' \n')"

curl -fs -X POST "$BASE/v1/image/convert?format=jpeg&width=100" -H 'Content-Type: image/png' \
    --data-binary @"$TMP/out.png" -o "$TMP/out2.jpg"
check "PNG -> JPEG signature" "ffd8ff" "$(head -c 3 "$TMP/out2.jpg" | od -An -tx1 | tr -d ' \n')"

# Text must render with the bundled fonts: a text-only SVG is not just a blank
# page, so the PNG is clearly bigger than an empty 300x80 white image (~300 bytes).
curl -fs -X POST "$BASE/v1/image/convert?format=png" -H 'Content-Type: image/svg+xml' \
    --data-binary @"$DIR/../testdata/text.svg" -o "$TMP/text.png"
SIZE="$(wc -c < "$TMP/text.png")"
if [ "$SIZE" -gt 1500 ]; then echo "  ok    SVG text renders ($SIZE bytes)"; else echo "  FAIL  SVG text looks empty ($SIZE bytes)"; FAILED=1; fi

check "JSON endpoint" "image/png" "$(curl -fs -X POST "$BASE/v1/image/convert-json?format=png" -H 'Content-Type: application/json' \
    -d '{"svg":"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\"/>"}' \
    | sed -n 's/.*"content_type":"\([^"]*\)".*/\1/p')"

check "bad format -> 400" "400" "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/v1/image/convert?format=gif" --data-binary @"$DIR/../testdata/simple.svg")"
check "garbage body -> 415" "415" "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/v1/image/convert?format=png" --data-binary 'hello')"
check "oversized output -> 422" "422" "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$BASE/v1/image/convert?format=png&width=9000" --data-binary @"$DIR/../testdata/simple.svg")"
check "unknown path -> 404" "404" "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/nope")"

if [ "$FAILED" -eq 0 ]; then
    echo "== all checks passed"
else
    echo "== FAILURES (container log follows)"
    docker logs "$NAME" 2>&1 | tail -20
fi
exit "$FAILED"

#!/bin/sh
# Builds the static Alpine nsh-pixelbridge image and extracts the compiled
# binary to disk -- see Dockerfile and docker/compile_alpine_static.sh for
# what the build itself does.
#
# Usage:
#   ./build.sh                          # build image, extract binary to ./nsh-pixelbridge
#   ./build.sh ./out/nsh-pixelbridge    # ...to a specific path instead
#
# IMAGE_TAG=nashcom/nsh-pixelbridge:0.9.0 ./build.sh   # override the image tag
# SWAGGER_UI=cdn ./build.sh      # /docs loads Swagger UI from a CDN instead of
#                                # embedding it (default: embed, works offline)
set -eu

IMAGE_TAG="${IMAGE_TAG:-nsh-pixelbridge:latest}"
SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
OUT="${1:-$SCRIPT_DIR/nsh-pixelbridge}"

# The argument is the OUTPUT file for the binary, not a script to run next.
# Refuse to overwrite anything that is not an earlier build output (an ELF
# binary): a mistyped "./build.sh tests/integration_test.sh" once replaced the
# test script with the binary.
if [ -f "$OUT" ] && [ "$(head -c 4 "$OUT" | od -An -tx1 | tr -d ' \n')" != "7f454c46" ]; then
    echo "ERROR: $OUT exists and is not a binary from an earlier build; refusing to overwrite it" >&2
    echo "       (the optional argument is where the extracted binary is written)" >&2
    exit 1
fi

docker build --progress=plain --build-arg SWAGGER_UI="${SWAGGER_UI:-embed}" -t "$IMAGE_TAG" "$SCRIPT_DIR"
echo "built image: $IMAGE_TAG"

CID="$(docker create "$IMAGE_TAG")"
docker cp "$CID:/nsh-pixelbridge" "$OUT"
docker rm "$CID" >/dev/null
echo "extracted binary to $OUT"

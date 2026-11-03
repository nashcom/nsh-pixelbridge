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

docker build --progress=plain --build-arg SWAGGER_UI="${SWAGGER_UI:-embed}" -t "$IMAGE_TAG" "$SCRIPT_DIR"
echo "built image: $IMAGE_TAG"

CID="$(docker create "$IMAGE_TAG")"
docker cp "$CID:/nsh-pixelbridge" "$OUT"
docker rm "$CID" >/dev/null
echo "extracted binary to $OUT"

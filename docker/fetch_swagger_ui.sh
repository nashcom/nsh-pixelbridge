#!/bin/sh
# Downloads the Swagger UI files that get compiled into the binary
# (SWAGGER_UI=embed, the default) and VERIFIES them against a pinned hash.
#
# The pin is the SHA-512 "integrity" value npm publishes for the exact
# swagger-ui-dist release (registry.npmjs.org/swagger-ui-dist/<version>,
# field dist.integrity). A different or tampered download fails the build.
# To upgrade: change VERSION and INTEGRITY together, and SWAGGER_UI_VERSION in
# src/docs.rs (a unit test checks both agree).
#
# Needs: curl, sha512sum, base64, od, tar (all in rust:alpine + apk add curl).
#
# Usage: docker/fetch_swagger_ui.sh [dest-dir]     (default: ./swagger-ui)
set -eu

VERSION=5.34.1
INTEGRITY='sha512-VSr8fXfI93obYaQicLJbLUKGRxDojj/SfRJDDLCioGTUBj4BK/Nj4eE9jCeCAnJxVLi7iS+08PbiZLyrQKBkJQ=='
URL="https://registry.npmjs.org/swagger-ui-dist/-/swagger-ui-dist-$VERSION.tgz"

DEST="${1:-swagger-ui}"

if [ -f "$DEST/.version" ] && [ "$(cat "$DEST/.version")" = "$VERSION" ]; then
    echo "swagger-ui $VERSION already in $DEST"
    exit 0
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "==> downloading swagger-ui-dist $VERSION"
curl -fsSL "$URL" -o "$TMP/pkg.tgz"

# npm's integrity is base64 of the SHA-512 digest; compare as hex.
EXPECTED="$(printf '%s' "${INTEGRITY#sha512-}" | base64 -d | od -An -v -tx1 | tr -d ' \n')"
ACTUAL="$(sha512sum "$TMP/pkg.tgz" | cut -d' ' -f1)"
if [ "$EXPECTED" != "$ACTUAL" ]; then
    echo "ERROR: swagger-ui-dist $VERSION does not match the pinned SHA-512" >&2
    echo "  expected $EXPECTED" >&2
    echo "  actual   $ACTUAL" >&2
    exit 1
fi
echo "==> SHA-512 matches the pinned value"

# Only what the UI page needs; the package has many more files (source maps, ES modules, ...).
tar -xzf "$TMP/pkg.tgz" -C "$TMP" package/swagger-ui-bundle.js package/swagger-ui.css package/LICENSE

rm -rf "$DEST"
mkdir -p "$DEST"
cp "$TMP/package/swagger-ui-bundle.js" "$TMP/package/swagger-ui.css" "$TMP/package/LICENSE" "$DEST/"
echo "$VERSION" > "$DEST/.version"
echo "==> swagger-ui $VERSION in $DEST ($(wc -c < "$DEST/swagger-ui-bundle.js") bytes of JavaScript)"

#!/bin/sh
# Runs the Rust unit and HTTP tests inside a throw-away rust:alpine container,
# so no Rust toolchain is needed on the host. The first run also creates
# Cargo.lock in the project directory -- commit it.
#
# The tests run twice, once per Swagger UI mode (embedded files / CDN page).
# The embedded mode needs ./swagger-ui, which the script downloads and verifies
# first (docker/fetch_swagger_ui.sh).
#
# Usage:
#   ./test.sh                  # all tests
#   ./test.sh -- --nocapture   # extra arguments go to `cargo test`
#
# The crate download cache lives in the docker volume "pixelbridge-cargo",
# build output in ./target (git-ignored).
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"

docker run --rm \
    -v "$SCRIPT_DIR":/src \
    -v pixelbridge-cargo:/usr/local/cargo/registry \
    -w /src \
    -e PIXELBRIDGE_TEST_FONT_DIR=/usr/share/fonts/dejavu \
    rust:alpine \
    sh -c 'apk add --no-cache musl-dev font-dejavu curl >/dev/null \
        && sh docker/fetch_swagger_ui.sh swagger-ui \
        && echo "==> tests with embedded Swagger UI (default features)" && cargo test "$@" \
        && echo "==> tests with CDN Swagger UI (--no-default-features)" && cargo test --no-default-features "$@"' sh "$@"

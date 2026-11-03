#!/bin/sh
# Builds a fully static nsh-pixelbridge binary. Meant to run inside an Alpine
# container that already has the Rust toolchain (see ../Dockerfile, based on
# rust:alpine) -- this script only compiles and verifies, it does not set up
# the environment, so it works the same from a Dockerfile RUN or by hand
# inside your own build container.
#
# Usage: docker/compile_alpine_static.sh [output-path]
# (default output path: ./nsh-pixelbridge, i.e. the project root)
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
PROJECT_ROOT="$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)"
OUT="${1:-$PROJECT_ROOT/nsh-pixelbridge}"

cd "$PROJECT_ROOT"

# Build for the host triple but name it explicitly (--target). Reason: with an
# explicit --target, RUSTFLAGS only applies to the final program and not to
# build scripts / proc-macros, which on a musl host must stay dynamically
# loadable. musl targets link statically by default; +crt-static states it
# outright so a changed default can never silently produce a dynamic binary.
TARGET="$(rustc -vV | sed -n 's/^host: //p')"
export RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+crt-static"

# CARGO_FLAGS is empty for the default build; the Dockerfile sets it to
# --no-default-features for SWAGGER_UI=cdn (word splitting is intended).
echo "==> building nsh-pixelbridge for $TARGET ${CARGO_FLAGS:-}"
# shellcheck disable=SC2086
cargo build --release --target "$TARGET" ${CARGO_FLAGS:-}

BIN="target/$TARGET/release/nsh-pixelbridge"

echo "==> verifying the binary is really static"
# A static executable has no dynamic loader (INTERP) and no needed libraries.
if readelf -lW "$BIN" | grep -q INTERP; then
    echo "ERROR: $BIN requests a dynamic loader (not static)" >&2
    exit 1
fi
if readelf -dW "$BIN" 2>/dev/null | grep -q NEEDED; then
    echo "ERROR: $BIN depends on shared libraries:" >&2
    readelf -dW "$BIN" | grep NEEDED >&2
    exit 1
fi
file "$BIN" 2>/dev/null || true

cp "$BIN" "$OUT"
echo "==> done: $OUT ($(wc -c < "$OUT") bytes)"

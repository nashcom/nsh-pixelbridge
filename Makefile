# nsh-pixelbridge - thin wrappers around cargo, for use inside a build
# container that has the Rust toolchain. Without one, use ./test.sh and
# ./build.sh, which bring their own container.

CARGO ?= cargo

.PHONY: all build test fmt clippy clean swagger-ui

all: build

build: swagger-ui
	$(CARGO) build --release

# Text rendering tests need DejaVu fonts, e.g. apk add font-dejavu
test: swagger-ui
	PIXELBRIDGE_TEST_FONT_DIR=$${PIXELBRIDGE_TEST_FONT_DIR:-/usr/share/fonts/dejavu} $(CARGO) test

fmt:
	$(CARGO) fmt

clippy:
	$(CARGO) clippy --all-targets -- -D warnings

clean:
	$(CARGO) clean
	rm -f nsh-pixelbridge

# The default build embeds Swagger UI; fetch (and hash-check) it first.
# Needs curl. For a build without it: cargo build --release --no-default-features
swagger-ui:
	sh docker/fetch_swagger_ui.sh swagger-ui

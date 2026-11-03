# Static Alpine/musl build of nsh-pixelbridge, packaged into a "FROM scratch"
# runtime image -- no base OS at all: the static binary, the bundled fonts,
# and nothing else.
#
# The compile and static-link verification live in
# docker/compile_alpine_static.sh (easier to run standalone in your own build
# container too).
#
# Fonts: SVG text needs fonts and a scratch image has none. Alpine's
# font-dejavu package supplies DejaVu (free license, redistributable); a few
# faces are copied to /fonts, which is where PIXELBRIDGE_FONT_DIR points.
#
# Architecture: the build uses the host's Rust target, so building on an
# ARM64 machine (or buildx with --platform linux/arm64) gives an ARM64 image
# without any change here.
ARG ALPINE_RUST_VERSION=alpine

FROM rust:${ALPINE_RUST_VERSION} AS build
RUN apk add --no-cache musl-dev file binutils font-dejavu curl

WORKDIR /src

# Swagger UI at /docs, chosen at build time:
#   embed (default): the UI files are downloaded here, checked against a pinned
#                    SHA-512 and compiled into the binary (about 1.5 MB more).
#   cdn            : nothing is added; /docs loads Swagger UI from a CDN in the
#                    viewer's browser.
#   docker build --build-arg SWAGGER_UI=cdn .      (or SWAGGER_UI=cdn ./build.sh)
ARG SWAGGER_UI=embed

# Fetched before the sources are copied, so this layer survives code changes.
COPY docker/fetch_swagger_ui.sh ./docker/
RUN case "$SWAGGER_UI" in \
        embed) sh ./docker/fetch_swagger_ui.sh swagger-ui ;; \
        cdn)   echo "SWAGGER_UI=cdn: nothing to download" ;; \
        *)     echo "ERROR: SWAGGER_UI must be 'embed' or 'cdn', not '$SWAGGER_UI'" >&2; exit 1 ;; \
    esac

# Cargo.lock is optional here so the very first build works; once it exists
# (./test.sh creates it) it is used and the build is reproducible.
COPY Cargo.toml Cargo.lock* ./
COPY openapi.json ./
COPY src ./src
COPY docker ./docker
RUN if [ "$SWAGGER_UI" = cdn ]; then export CARGO_FLAGS="--no-default-features"; fi; \
    sh ./docker/compile_alpine_static.sh

RUN mkdir -p /fonts-out/licenses \
 && for f in DejaVuSans DejaVuSans-Bold DejaVuSans-Oblique DejaVuSerif DejaVuSerif-Bold DejaVuSansMono; do \
        cp "/usr/share/fonts/dejavu/$f.ttf" /fonts-out/; \
    done \
 && (cp -r /usr/share/licenses/font-dejavu/. /fonts-out/licenses/ 2>/dev/null \
     || cp -r /usr/share/doc/font-dejavu*/. /fonts-out/licenses/ 2>/dev/null \
     || echo "WARNING: no DejaVu license text found in the font package") \
 && chmod -R a+rX /fonts-out

FROM scratch AS runtime

COPY --from=build /src/nsh-pixelbridge /nsh-pixelbridge
COPY --from=build /fonts-out /fonts
COPY NOTICE /NOTICE
COPY LICENSE /LICENSE

# Links the GHCR package to the repository (and shows the license there).
LABEL org.opencontainers.image.title="nsh-pixelbridge" \
      org.opencontainers.image.description="SVG/PNG/JPEG to PNG/JPEG converter REST service (static binary, scratch image)" \
      org.opencontainers.image.source="https://github.com/nashcom/nsh-pixelbridge" \
      org.opencontainers.image.licenses="Apache-2.0"

ENV PIXELBRIDGE_LISTEN=0.0.0.0:8080 \
    PIXELBRIDGE_FONT_DIR=/fonts

USER 1000:1000

EXPOSE 8080

ENTRYPOINT ["/nsh-pixelbridge"]

HEALTHCHECK --interval=60s --timeout=3s --start-period=5s --retries=3 \
    CMD ["/nsh-pixelbridge", "-health-check"]

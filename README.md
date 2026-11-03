# nsh-pixelbridge

[![CI](https://github.com/nashcom/nsh-pixelbridge/actions/workflows/ci.yml/badge.svg)](https://github.com/nashcom/nsh-pixelbridge/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

**PixelBridge** is a small REST service that **converts and resizes images**.
SVG, PNG or JPEG goes in, PNG or JPEG comes out.

It is built for two kinds of callers:

- **AI clients** (through an OpenAPI-to-MCP bridge): the image travels as text,
  SVG source or base64, and the result comes back as base64 JSON.
- **curl, scripts and applications**: raw image bytes in, raw image bytes out.

The main deliverable is a **container image** with a single static binary and
nothing else in it, in the same style as `nshgeoip` and `nshmqtt`.

| Topic           | Details                                                     |
|-----------------|-------------------------------------------------------------|
| Language        | Rust (axum, tokio, resvg, image)                            |
| Image           | `FROM scratch`: static musl binary + six fonts, non-root    |
| Size            | about 8.5 MB, plus about 1.5 MB with embedded Swagger UI    |
| Protocol        | plain HTTP/1.1; NGINX in front does TLS and access control  |
| API description | OpenAPI 3.0.3 at `/openapi.json`, Swagger UI at `/docs`     |
| Status          | 0.9.x: complete and tested; API may still change before 1.0 |

---

## Contents

1. [Quick start](#quick-start)
2. [How it fits together](#how-it-fits-together)
3. [Using the API](#using-the-api)
4. [Swagger UI](#swagger-ui)
5. [Using it from an MCP bridge](#using-it-from-an-mcp-bridge)
6. [Deployment](#deployment)
7. [Configuration](#configuration)
8. [Security model](#security-model)
9. [SVG text and fonts](#svg-text-and-fonts)
10. [Troubleshooting](#troubleshooting)
11. [Limitations](#limitations)
12. [Development](#development)
13. [Releases and the published image](#releases-and-the-published-image)
14. [Design decisions](#design-decisions)
15. [Repository layout](#repository-layout)
16. [License](#license)

---

## Quick start

You need Docker and nothing else: no Rust toolchain on the host.

```bash
./test.sh                 # all tests, in a throw-away rust:alpine container (also creates Cargo.lock)
./build.sh                # builds the image nsh-pixelbridge:latest (and extracts ./nsh-pixelbridge)
docker compose up -d      # starts it; port from .env (copy .env.example), default 8080
```

Or run the published image directly:

```bash
docker run -d --name nsh-pixelbridge \
  --read-only --cap-drop ALL --security-opt no-new-privileges \
  -p 127.0.0.1:8080:8080 \
  ghcr.io/nashcom/nsh-pixelbridge:latest
```

Try it:

```bash
curl -fsS http://localhost:8080/health

curl -fsS -X POST 'http://localhost:8080/v1/image/convert?format=png&width=512' \
     -H 'Content-Type: image/svg+xml' --data-binary @testdata/simple.svg -o result.png
```

Then open **http://localhost:8080/docs** for the interactive API
([Swagger UI](#swagger-ui)). `examples/curl-example.sh` runs a complete tour
and uses the port from `.env`.

End-to-end test of the built image, started the way production does it
(read-only root, non-root user, no capabilities):

```bash
tests/integration_test.sh
```

---

## How it fits together

```
   AI client (Claude, OpenCode, ...)            curl, scripts, applications
                  |                                          |
                  v                                          |
   OpenAPI-to-MCP bridge (nsh-openapi-mcp)                   |
                  |                                          |
                  +------------------+-----------------------+
                                     |
                                     v
                          NGINX  (TLS, allow list, size limit)
                                     |   plain HTTP
                                     v
        +--------------------- nsh-pixelbridge ---------------------+
        |                                                           |
        |   axum / tokio        routes, parameters, JSON errors     |
        |        |                                                  |
        |        |  one conversion slot per request (bounded)       |
        |        v                                                  |
        |   worker thread       sniff -> render/decode -> resize    |
        |                       -> flatten background -> encode     |
        |        |                                                  |
        |        +-- resvg / usvg / tiny-skia   SVG rasterizer      |
        |        +-- image crate                PNG + JPEG codecs   |
        |                                                           |
        |   no files, no network, nothing written to disk           |
        +-----------------------------------------------------------+
```

The service has **no MCP code**. The OpenAPI document is all a bridge needs, so
the same service works for humans, scripts and AI clients, and a bridge change
never requires a new image.

---

## Using the API

The machine-readable description is `GET /openapi.json`; `GET /docs` shows it as
Swagger UI.

| Endpoint                      | Purpose                                                 |
|-------------------------------|---------------------------------------------------------|
| `POST /v1/image/convert-json` | Image in a JSON body, result as base64 JSON (MCP, AI)   |
| `POST /v1/image/convert`      | Raw image bytes in, raw image bytes out (curl, scripts) |
| `GET  /health`                | `{"status":"ok","version":"..."}`; no authentication    |
| `GET  /openapi.json`          | The OpenAPI 3.0.3 document                              |
| `GET  /docs`                  | Swagger UI ([details](#swagger-ui))                     |

### Conversion options

The **same five options** are **query parameters on both conversion endpoints**
(which is also why Swagger UI shows them as separate input fields):

| Option       | Values                           | Notes                                          |
|--------------|----------------------------------|------------------------------------------------|
| `format`     | `png`, `jpeg`                    | **Required.** Output format                    |
| `width`      | 1 up to the configured maximum   | Pixels; see [sizing](#sizing)                  |
| `height`     | 1 up to the configured maximum   | Pixels; see [sizing](#sizing)                  |
| `background` | `transparent`, `#rgb`, `#rrggbb` | PNG keeps transparency; JPEG defaults to white |
| `quality`    | 1 to 100 (default 90)            | JPEG only; refused for PNG                     |

- `transparent` is refused for JPEG, which has no transparency.
- In a hand-written URL write `#` as `%23` (Swagger UI does this for you).
- Unknown parameters are errors, so a typo such as `widht=100` cannot go
  unnoticed.

#### Sizing

| You give      | You get                                             |
|---------------|-----------------------------------------------------|
| nothing       | the image's own size, rounded up                    |
| `width` only  | that width; the height follows the aspect ratio     |
| `height` only | that height; the width follows the aspect ratio     |
| both          | fitted inside the box, aspect kept, never stretched |

With both given, the result can be smaller than the box in one dimension. Small
images are scaled up if you ask for a larger size. An SVG without a usable size
falls back to its viewBox. Anything beyond the configured limits is refused
**before** any pixel buffer is allocated.

### Raw bytes: `POST /v1/image/convert`

The request body is the image file. The `Content-Type` header is optional; if
present it must be `image/svg+xml`, `image/png`, `image/jpeg` or
`application/octet-stream`, and it must agree with the bytes. curl's default
`application/x-www-form-urlencoded` (sent when `--data-binary` has no `-H`)
counts as not given. The format is **always detected from the data**, never
trusted from the header. The response body is the converted image, with
`Content-Type`, `Content-Length`, `X-Image-Width` and `X-Image-Height`.

```bash
# SVG to PNG, 512 pixels wide
curl -fsS -X POST 'http://localhost:8080/v1/image/convert?format=png&width=512' \
     -H 'Content-Type: image/svg+xml' --data-binary @logo.svg -o logo.png

# SVG to JPEG; transparent areas become white (or pick a color: &background=%23eeeeee)
curl -fsS -X POST 'http://localhost:8080/v1/image/convert?format=jpeg&width=512&quality=85' \
     -H 'Content-Type: image/svg+xml' --data-binary @logo.svg -o logo.jpg

# Fit inside a 256 x 256 box (an SVG of 100 x 50 comes out 256 x 128)
curl -fsS -X POST 'http://localhost:8080/v1/image/convert?format=png&width=256&height=256' \
     --data-binary @logo.svg -o thumb.png

# Resize a photo: JPEG to JPEG, 800 pixels wide
curl -fsS -X POST 'http://localhost:8080/v1/image/convert?format=jpeg&width=800&quality=85' \
     -H 'Content-Type: image/jpeg' --data-binary @photo.jpg -o small.jpg

# PNG with a solid background instead of transparency
curl -fsS -X POST 'http://localhost:8080/v1/image/convert?format=png&background=%23ffffff' \
     --data-binary @logo.svg -o logo-white.png
```

### JSON: `POST /v1/image/convert-json`

The options are the query parameters above. The **JSON body carries only the
image**: exactly one of

- `svg`: SVG source text, no encoding needed
- `data_base64`: base64 of an SVG, PNG or JPEG file (a `data:...;base64,`
  prefix is accepted)

Options inside the body are refused with a message that points to the query
parameters.

```bash
curl -fsS -X POST 'http://localhost:8080/v1/image/convert-json?format=png&width=256' \
  -H 'Content-Type: application/json' \
  -d '{"svg":"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"100\" height=\"50\"><rect width=\"100\" height=\"50\" fill=\"#3366cc\"/></svg>"}'
```

Response:

```json
{"format":"png","content_type":"image/png","width":256,"height":128,"size_bytes":812,"data_base64":"iVBORw0KGgo..."}
```

Save the result as a file:

```bash
curl -fsS -X POST 'http://localhost:8080/v1/image/convert-json?format=png&width=256' \
     -H 'Content-Type: application/json' -d '{"svg":"<svg ...>"}' \
  | jq -r .data_base64 | base64 -d > result.png
```

Send a binary image as base64:

```bash
B64=$(base64 -w0 photo.png)
curl -fsS -X POST 'http://localhost:8080/v1/image/convert-json?format=jpeg&width=400' \
     -H 'Content-Type: application/json' -d "{\"data_base64\":\"$B64\"}"
```

### Errors

Always JSON, and never internals (no stack traces, paths or parser messages):

```json
{"error":"invalid_svg","message":"SVG input could not be parsed"}
```

| Status | `error`                                                              | Meaning                                 |
|--------|----------------------------------------------------------------------|-----------------------------------------|
| 400    | `invalid_parameter`                                                  | Query parameter missing, unknown or bad |
| 400    | `invalid_json`, `invalid_base64`, `empty_body`                       | Request body unusable                   |
| 400    | `invalid_svg`, `invalid_image`, `unsupported_svg`, `format_mismatch` | Input unusable                          |
| 404    | `not_found`                                                          | No such endpoint                        |
| 413    | `payload_too_large`                                                  | Over `PIXELBRIDGE_MAX_REQUEST_BYTES`    |
| 415    | `unsupported_media_type`                                             | Not SVG, PNG or JPEG                    |
| 422    | `image_too_large`                                                    | Input or output over the size limits    |
| 429    | `busy`                                                               | All conversion slots in use; retry      |
| 500    | `internal_error`                                                     | A bug; the reason is in the log         |
| 504    | `timeout`                                                            | Over `PIXELBRIDGE_TIMEOUT_SECONDS`      |

- `unsupported_svg` means the SVG contains entity declarations (`<!ENTITY`).
- `format_mismatch` means the `Content-Type` says one format and the bytes are
  another.

---

## Swagger UI

Open `http://localhost:<port>/docs`. It shows the OpenAPI document and has a
**Try it out** button for every call, with one input field per option.

How the UI files reach the browser is a **build-time choice**:

| Build             | Command                     | Effect                                                    |
|-------------------|-----------------------------|-----------------------------------------------------------|
| `embed` (default) | `./build.sh`                | UI compiled into the binary (about 1.5 MB); works offline |
| `cdn`             | `SWAGGER_UI=cdn ./build.sh` | Browser loads the UI from `cdn.jsdelivr.net`              |

- **embed:** the files are downloaded during the image build and **checked
  against a pinned SHA-512**. The browser contacts nobody else.
- **cdn:** nothing is added to the binary, but the viewer's browser needs
  internet access and trusts that CDN.
- With compose: `SWAGGER_UI=cdn docker compose build`.
- The page uses relative URLs, so it also works when NGINX publishes the
  service under a path prefix (open `.../docs` **without** a trailing slash).
- Like everything else, `/docs` has no authentication: restrict access in
  NGINX.

---

## Using it from an MCP bridge

Point the OpenAPI-to-MCP bridge at `http(s)://<host>/openapi.json`. Set
`PIXELBRIDGE_PUBLIC_URL` so the document names the server the bridge should call.

The bridge sees two tools (the operation ids from the OpenAPI document):

| Tool               | Use it for                                                  |
|--------------------|-------------------------------------------------------------|
| `convertImageJson` | **AI clients**: options as parameters, body `svg` or base64 |
| `convertImage`     | Raw bytes in and out: HTTP clients, **not** MCP             |

Good to know:

- The result's `data_base64` is *data*. Whether a client shows it as an
  **image** depends on the bridge turning `content_type` plus `data_base64`
  into an MCP image content block. That is a bridge feature and not part of
  this service. (At the time of writing, `nsh-openapi-mcp` has no image or
  binary handling.)
- An AI client can pass SVG source directly in `svg`: no base64 is needed on the
  way in, which keeps prompts and tool calls small.
- Errors are JSON with a stable `error` code and a message written for the
  caller, so a model can correct a wrong parameter by itself.

---

## Deployment

### Compose

`compose.yml` runs the image the way it is meant to be run:

- read-only root filesystem, all capabilities dropped, `no-new-privileges`
- memory, CPU and pid limits (a conversion is CPU and memory work on untrusted input)
- published on **127.0.0.1** only, port from `.env` (`PIXELBRIDGE_PORT`)
- container healthcheck with `/nsh-pixelbridge -health-check`

```bash
docker compose up -d
docker compose logs -f nsh-pixelbridge
```

### NGINX in front

PixelBridge speaks plain HTTP and has no authentication of its own. Put NGINX
in front for TLS, an allow list and request limits; `examples/nginx-pixelbridge.conf`
is a starting point:

- `client_max_body_size` at least `PIXELBRIDGE_MAX_REQUEST_BYTES`, plus about a
  third more for the JSON endpoint (base64)
- `allow` / `deny` for the callers that may use it (the MCP bridge host)
- `proxy_read_timeout` a little above `PIXELBRIDGE_TIMEOUT_SECONDS`
- `limit_req` if callers could flood it
- request buffering on, so slow clients are absorbed by NGINX

### Go-live checklist

- [ ] NGINX (or equivalent) terminates TLS and **restricts who can call** the service
- [ ] The container port is bound to `127.0.0.1` or a private network, not `0.0.0.0` on the host
- [ ] `/docs` and `/openapi.json` are covered by the same access rules (or blocked in NGINX)
- [ ] `client_max_body_size` matches `PIXELBRIDGE_MAX_REQUEST_BYTES` (+ about 33 % for JSON)
- [ ] Container `mem_limit`, `cpus` and `pids_limit` are set, and
      `MAX_PIXELS * 8 * MAX_CONCURRENT` is well below the memory limit
- [ ] `PIXELBRIDGE_PUBLIC_URL` is set if a bridge reads the OpenAPI document
- [ ] `PIXELBRIDGE_OPENAPI_FILE` is **not** set (testing aid only)
- [ ] The image is pinned to a version tag or digest, not `:latest`
- [ ] Logs are collected (`docker logs`); watch for `429 busy` and `504 timeout`
- [ ] You decided between the embedded and the CDN [Swagger UI](#swagger-ui)

---

## Configuration

Environment variables only (no config file), all validated at startup: an
invalid value stops the service with a clear message.

| Variable                        | Default           | Range             | Meaning                                     |
|---------------------------------|-------------------|-------------------|---------------------------------------------|
| `PIXELBRIDGE_LISTEN`            | `0.0.0.0:8080`    | ip:port           | Listen address (plain HTTP)                 |
| `PIXELBRIDGE_FONT_DIR`          | `/fonts`          | directory         | Fonts for SVG text                          |
| `PIXELBRIDGE_MAX_REQUEST_BYTES` | `5242880` (5 MiB) | 1024 to 268435456 | Maximum image size in a request             |
| `PIXELBRIDGE_MAX_DIMENSION`     | `4096`            | 1 to 16384        | Maximum output width and height             |
| `PIXELBRIDGE_MAX_PIXELS`        | `16000000`        | 1 to 268435456    | Maximum output pixels, and PNG/JPEG input   |
| `PIXELBRIDGE_MAX_CONCURRENT`    | `4`               | 1 to 256          | Conversions running at the same time        |
| `PIXELBRIDGE_TIMEOUT_SECONDS`   | `15`              | 1 to 600          | How long a request waits for its result     |
| `PIXELBRIDGE_PUBLIC_URL`        | none              | URL               | Server URL advertised in `/openapi.json`    |
| `PIXELBRIDGE_OPENAPI_FILE`      | none              | file path         | Serve `/openapi.json` from a file (testing) |
| `RUST_LOG`                      | `info`            | level             | `error`, `warn`, `info`, `debug`, `trace`   |

Command line (single dash, like the other NashCom tools):

| Option          | What it does                                                |
|-----------------|-------------------------------------------------------------|
| `-help`         | usage and the settings above                                |
| `-version`      | prints the version                                          |
| `-health-check` | probes `GET /health`; exit 0 when healthy (container check) |

`-health-check` exists because the image has no curl.

**Memory planning.** One conversion needs roughly `MAX_PIXELS * 4` bytes for the
result plus about the same again for working copies. Keep
`MAX_PIXELS * 8 * MAX_CONCURRENT` well below the container memory limit
(defaults: 16 M pixels and 4 slots come to about 512 MB).

**Editing the API description without a rebuild.** `openapi.json` is built into
the binary. To test changes (examples, descriptions, parameters) set
`PIXELBRIDGE_OPENAPI_FILE` and mount the file read-only (the two lines are in
`compose.yml`, commented out). The service then reads the file on **every
request**: edit it and reload `/docs`. The file is checked at startup (a wrong
path or broken JSON stops the service). If it breaks later, `/openapi.json`
answers HTTP 500 with the reason in the log, instead of silently showing the
built-in copy. `PIXELBRIDGE_PUBLIC_URL` still applies. Not meant for production.

### Logging

One line per conversion, and one per rejected request. **Image data is never
logged.** (The sizes and timings below are examples.)

```
INFO  convert: asked png width=512 height=-: 5102 bytes in -> png 512x99 10217 bytes out in 2 ms
INFO  convert-json: asked jpeg width=256 height=-: 14336 bytes in -> jpeg 256x69 6120 bytes out in 3 ms
WARN  POST /v1/image/convert: 400 invalid_parameter: format must be one of: png, jpeg
WARN  GET /favicon.ico: 404 not_found: no such endpoint
```

Rejected requests are logged at `warn` (internal errors at `error`) with the
method, the path (never the query string), the status, the error code and the
first 200 characters of the message. The parser's own error text is logged at
`debug` only and is never shown to clients.

---

## Security model

All input is **untrusted**. PixelBridge has **no authentication**: restrict who
can reach it with NGINX, network policy or a firewall.

### What is enforced

- **Huge uploads.** A request size limit; the body is read with a hard cap.
- **Huge images and decompression bombs.** Output width, height and pixel
  limits. The size of a PNG/JPEG **input** is read from its header **before
  decoding**, so a small file that claims a gigantic image is refused before
  anything is allocated.
- **CPU and memory exhaustion.** A fixed number of conversion slots; the next
  caller gets `429` immediately instead of queueing unbounded work. A render
  cannot be interrupted once running, so a timed-out conversion **keeps its
  slot until it really ends**. Container CPU, memory and pid limits are the
  second line (see `compose.yml`).
- **SVG reading local files.** `<image>` references to files, URLs or relative
  paths are **not loaded**: the renderer's file loading is replaced by a
  resolver that always declines, and a test proves that a real PNG on disk is
  not drawn. System fonts are not read.
- **SVG network access.** None: nothing is fetched.
- **SVG scripts and active content.** Not executed; only the static subset is
  rendered.
- **Embedded `data:` images.** Allowed, but their pixel size is checked first.
  Only PNG and JPEG can be embedded; GIF and WebP are dropped.
- **XML "billion laughs".** `<!ENTITY` declarations are refused. gzip-compressed
  SVG (`.svgz`) is not accepted.
- **Type confusion.** The format is detected from the **bytes**; a mismatching
  `Content-Type` is rejected.
- **Disk access.** Nothing is written; everything is in memory. The image has a
  read-only root filesystem and runs as UID 1000.
- **Information leaks.** Errors are fixed texts with a stable code; stack
  traces, paths and parser messages never reach the client.
- **A decoder bug.** A panic fails that one request (HTTP 500); the service
  keeps running.
- **Secrets in logs.** Image data is never logged; messages are cut to 200
  characters.

### What is not covered (by design)

- **Authentication and authorization.** Use NGINX, a private network or both.
- **Per-client rate limiting.** The slot limit protects the service as a whole;
  use `limit_req` in NGINX to protect it from one noisy client.
- **Slow-client protection.** NGINX request buffering absorbs slow uploads.
- **TLS.** NGINX terminates it.
- **A metrics endpoint.** Use the logs (and the container healthcheck).
- **Heavy but valid SVGs** (large blur filters and similar) are bounded by the
  timeout, the slot limit and the container limits, not detected in advance.

---

## SVG text and fonts

A `scratch` image has no fonts, so the image bundles **DejaVu Sans (regular,
bold, oblique), DejaVu Serif (regular, bold) and DejaVu Sans Mono** in `/fonts`
(free license; see `NOTICE` and `/fonts/licenses`). They cover Latin, Greek,
Cyrillic and many symbols (for example `€`), but **no CJK**.

- Text without a `font-family` uses DejaVu Sans; the generic families
  `sans-serif`, `serif` and `monospace` map to DejaVu Sans, Serif and Sans Mono.
- A family that is not available falls back to the generic or default family;
  characters that no bundled font has may be left out.
- Fonts are **never downloaded**. To add fonts, mount a directory with
  `.ttf` / `.otf` files and set `PIXELBRIDGE_FONT_DIR`.
- **Tip for logos:** export the SVG with text converted to paths (as the
  NashCom logo in `examples/` is). The result then looks identical
  everywhere and needs no fonts at all.

---

## Troubleshooting

- **`400 invalid_parameter: format must be one of: png, jpeg`**: `format` is
  missing or not `png`/`jpeg`. WebP and GIF are not supported.
- **`400 invalid_parameter` naming an unknown field**: a misspelled or
  unsupported option. Options are query parameters, not body fields.
- **`400 invalid_json` on `convert-json`**: the body must be JSON with `svg`
  **or** `data_base64`; options such as `format` and `width` belong in the
  query string.
- **`background=#ffffff` has no effect or fails**: in a hand-written URL write
  `#` as `%23`. Swagger UI does this for you.
- **`400 invalid_svg`**: the SVG could not be parsed. Set `RUST_LOG=debug` and
  look for `SVG parse failed` for the parser's reason.
- **`400 format_mismatch`**: the `Content-Type` says one format, the bytes are
  another. Fix the header or leave it out.
- **`413 payload_too_large`**: over `PIXELBRIDGE_MAX_REQUEST_BYTES`; also check
  NGINX `client_max_body_size`.
- **`422 image_too_large`**: the requested output (or a PNG/JPEG input) is over
  `PIXELBRIDGE_MAX_DIMENSION` / `PIXELBRIDGE_MAX_PIXELS`.
- **`429 busy`**: all slots are in use. Retry, or raise
  `PIXELBRIDGE_MAX_CONCURRENT` if memory allows.
- **`504 timeout`**: the conversion took longer than
  `PIXELBRIDGE_TIMEOUT_SECONDS`.
- **SVG text is missing or in the wrong font**: the font is not bundled (see
  [fonts](#svg-text-and-fonts)); the startup log says how many font faces were
  loaded.
- **An image inside the SVG does not show**: `<image>` references to files and
  URLs are blocked on purpose; embed it as a `data:` PNG/JPEG.
- **The JPEG has a white background**: JPEG has no transparency; pass
  `background=%23rrggbb` for another color.
- **`/docs` is blank**: with `SWAGGER_UI=cdn` the browser must reach
  `cdn.jsdelivr.net`; the default `embed` build needs no internet.
- **`/docs/` (trailing slash) gives 404**: open `/docs` without the trailing
  slash.
- **The container exits at startup**: a setting is invalid; `docker logs` shows
  which one and the allowed range.

Useful commands:

```bash
docker logs -f nsh-pixelbridge                        # follow the log
docker exec nsh-pixelbridge /nsh-pixelbridge -health-check ; echo $?
RUST_LOG=debug docker compose up -d --force-recreate  # parser reasons and more detail
```

---

## Limitations

- **Output formats:** PNG and JPEG only. WebP and GIF are not supported (lossy
  WebP needs a C library, which conflicts with the static `scratch` goal).
- **Input formats:** SVG, PNG and JPEG. JPEG EXIF orientation is ignored.
- **Metadata** (EXIF, ICC profiles) is not carried over to the output.
- **SVG:** only the static subset is rendered (no scripts, no animation); no
  external images or fonts.
- **No authentication, no TLS:** by design, NGINX does both.
- **HTTP/1.1 only** (what NGINX uses to talk to upstreams anyway).
- The service is **stateless**: no storage, no cache, no job queue.

---

## Development

You need Docker. The scripts bring their own containers.

- **`./test.sh`** runs the unit and HTTP tests in a `rust:alpine` container,
  **twice**: once with the embedded Swagger UI and once with the CDN variant.
  It downloads and verifies the Swagger UI files first and creates
  `Cargo.lock`. Extra arguments go to `cargo test` (`./test.sh -- --nocapture`).
- **`./build.sh`** builds the image and extracts the static binary to
  `./nsh-pixelbridge`. `IMAGE_TAG=...` renames the image, `SWAGGER_UI=cdn`
  selects the CDN variant.
- **`tests/integration_test.sh`** starts the built image read-only, non-root and
  without capabilities, then checks health, `/docs`, OpenAPI, SVG to PNG and
  JPEG, PNG to JPEG, SVG text rendering, the JSON endpoint and the error
  statuses. It uses `127.0.0.1:18089` and refuses to run if something already
  answers there (`PORT=...` changes it).
- **`examples/curl-example.sh`** is a tour of the API against a running
  instance; it reads the port from `.env`.
- **`make build` / `make test`** do the same inside a build container that
  already has Rust (it needs curl for the Swagger UI files).

**What the tests cover:** sizing rules (width, height, both), transparency and
background handling, JPEG flattening on white, PNG/JPEG input, SVG text with the
bundled fonts, every error path, request/pixel/concurrency limits, proof that
SVG file and URL references are not loaded, entity-declaration refusal, both
conversion endpoints, `/openapi.json` (binary schemas, the shared query
parameters, the optional file mode), `/docs` in both Swagger UI modes, and
consistency checks between the code and the documents (OpenAPI version,
Swagger UI version).

**The static build** (`Dockerfile` and `docker/compile_alpine_static.sh`):
`rust:alpine` build stage, compile, then **verify** that the binary has no
dynamic loader and no shared-library dependency (the build fails otherwise),
then `FROM scratch` with the binary and the fonts. The build targets the
machine's own architecture, so building on ARM64 gives an ARM64 image.

**Swagger UI files:** `docker/fetch_swagger_ui.sh` downloads `swagger-ui-dist`
into `./swagger-ui` and aborts unless the download matches the pinned SHA-512.
The Cargo feature `swagger-embed` (on by default) compiles it in;
`--no-default-features` gives the CDN variant. To upgrade, change the version
and hash in that script and `SWAGGER_UI_VERSION` in `src/docs.rs` (a unit test
checks that they agree). A plain `cargo build` needs `./swagger-ui` to exist
first (`make swagger-ui`).

**`Cargo.lock` is committed** (this is an application, not a library). Run
`./test.sh` once to create it.

---

## Releases and the published image

`.github/workflows/` follows the other NashCom projects:

| Workflow      | Runs                        | Does                                            |
|---------------|-----------------------------|-------------------------------------------------|
| `ci.yml`      | push and pull request       | `./test.sh`, `./build.sh`, integration test     |
| `release.yml` | GitHub release is published | builds amd64 + arm64, attaches binaries, pushes |

`ci.yml` runs the same scripts you run locally. `release.yml` attaches the
binaries with `.sha256` files to the release and pushes a multi-arch image to
GHCR:

```bash
docker pull ghcr.io/nashcom/nsh-pixelbridge:latest      # or :<version>
```

**To release:**

1. Bump `version` in `Cargo.toml` **and** `info.version` plus the health
   example in `openapi.json` (a unit test fails if they differ).
2. Commit and push.
3. Publish a GitHub release whose tag is `v<version>`. The workflow refuses a
   tag that does not match `Cargo.toml`.

Each architecture is built on a **native** GitHub runner (`ubuntu-latest` and
`ubuntu-24.04-arm`) instead of QEMU, because compiling Rust with full LTO under
emulation is far too slow. The two single-architecture images are pushed by
digest and then joined into one manifest.

By hand, for a multi-arch image:

```bash
docker buildx build --platform linux/amd64,linux/arm64 -t nashcom/nsh-pixelbridge:0.9.0 --push .
```

---

## Design decisions

- **`FROM scratch`, no base image.** There is nothing to patch or scan: no
  shell, no package manager, no libc. The runtime is one static binary plus six
  fonts.
- **Rust with resvg.** A pure-Rust SVG renderer links statically without C
  libraries, and a memory-safe language suits code that parses untrusted files.
- **No MCP code in the service.** The OpenAPI document is the integration
  point; the same service serves humans, scripts and AI clients.
- **Options as query parameters on both endpoints.** One definition for both;
  Swagger UI and MCP bridges show them as separate fields; the body carries only
  the image.
- **Base64 JSON next to raw bytes.** AI clients cannot send or receive binary
  bodies; scripts should not pay for base64.
- **PNG and JPEG only.** Both encode in pure Rust. Lossy WebP needs a C library,
  which would break the static build.
- **No TLS, no authentication.** NGINX already does both in the NashCom setups;
  less code in the service means less to audit.
- **Environment variables only.** That is how containers are configured; every
  setting has a default and a validated range.
- **Bounded slots and an immediate 429.** A queue would hide overload and hold
  memory; a fast refusal lets the caller decide.
- **Native arm64 runners for releases.** Rust with full LTO under QEMU emulation
  would take hours.
- **Swagger UI embedded by default, CDN optional.** Offline-capable and no third
  party in the browser by default; the CDN variant gives the smallest image.

---

## Repository layout

```
nsh-pixelbridge/
├── Cargo.toml  Cargo.lock
├── openapi.json                 built into the binary, served at /openapi.json
├── src/
│   ├── main.rs                  startup, signals, -health-check
│   ├── lib.rs
│   ├── config.rs                PIXELBRIDGE_* environment settings
│   ├── api.rs                   routes, parameters, limits, worker hand-off, logging
│   ├── convert.rs               sniffing, SVG render, resize, encode
│   ├── docs.rs                  Swagger UI page at /docs (embedded or CDN)
│   └── errors.rs                one JSON error type
├── tests/
│   ├── api.rs                   HTTP-level tests (cargo test)
│   └── integration_test.sh      tests against the built container image
├── testdata/                    sample SVGs for the tests
├── examples/                    curl tour, NGINX config, sample logo
├── docker/                      compile_alpine_static.sh, fetch_swagger_ui.sh
├── .github/workflows/           ci.yml, release.yml
├── Dockerfile  build.sh  test.sh  Makefile  compose.yml  .env.example
└── README.md  LICENSE  NOTICE
```

---

## License

Apache License 2.0, see [LICENSE](LICENSE) and [NOTICE](NOTICE).

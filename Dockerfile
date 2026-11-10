FROM rust:1.96.1-bookworm AS build
WORKDIR /source
COPY rust-toolchain.toml Cargo.toml Cargo.lock ./
COPY server ./server
COPY web ./web
COPY schemas ./schemas
RUN rustup target add wasm32-unknown-unknown \
    && cargo install wasm-bindgen-cli --version 0.2.130 --locked \
    && cargo build --release --locked -p newphoto \
    && cargo build --profile wasm-release --locked -p newphoto-web --target wasm32-unknown-unknown \
    && mkdir -p /web/pkg \
    && wasm-bindgen --target web --out-dir /web/pkg --out-name newphoto target/wasm32-unknown-unknown/wasm-release/newphoto-web.wasm \
    && cp web/index.html web/app.js web/style.css web/theme.js web/bootstrap.js /web/

FROM debian:12.12-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 newphoto \
    && useradd --uid 10001 --gid 10001 --create-home newphoto \
    && mkdir -p /app/data /app/web && chown -R 10001:10001 /app
WORKDIR /app
COPY --from=build /source/target/release/newphoto /usr/local/bin/newphoto
COPY --from=build /web /app/web
USER 10001:10001
ENV NEWPHOTO_BIND=0.0.0.0:8080 NEWPHOTO_DATA_DIR=/app/data NEWPHOTO_WEB_DIR=/app/web
EXPOSE 8080
HEALTHCHECK --interval=20s --timeout=3s --start-period=15s --retries=3 CMD curl --fail --silent http://127.0.0.1:8080/health/ready || exit 1
CMD ["newphoto", "serve"]

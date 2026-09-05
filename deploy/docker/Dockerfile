# syntax=docker/dockerfile:1.7
# Multi-stage build: dashboard, then daemon, then a minimal runtime image.
# Multi-arch friendly: build with `docker buildx build --platform linux/amd64,linux/arm64`.

FROM --platform=$BUILDPLATFORM node:22-alpine AS ui
WORKDIR /src/web
COPY web/package.json web/package-lock.json* ./
RUN npm ci
COPY web/ ./
RUN npm run build

FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock* rust-toolchain.toml ./
COPY crates/ crates/
COPY --from=ui /src/web/dist web/dist
RUN cargo build --release -p alamo && strip target/release/alamo

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home /var/lib/alamo alamo \
 && mkdir -p /var/lib/alamo && chown alamo:alamo /var/lib/alamo
COPY --from=build /src/target/release/alamo /usr/local/bin/alamo
USER alamo
WORKDIR /var/lib/alamo
EXPOSE 3333 8080
ENTRYPOINT ["alamo"]
CMD ["--config", "/etc/alamo/alamo.toml"]

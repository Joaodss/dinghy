# syntax=docker/dockerfile:1

# --- Frontend (same output on every platform, so build it once natively) ----
FROM --platform=$BUILDPLATFORM node:24-alpine AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

# --- Server (static musl binary with the frontend embedded) ---------------
FROM rust:1.99-alpine AS server
RUN apk add --no-cache musl-dev gcc
WORKDIR /src/server
# Build dependencies first so they are cached across source changes.
COPY server/Cargo.toml server/Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && touch src/lib.rs \
    && cargo build --release --locked && rm -rf src
COPY server/ ./
COPY --from=web /src/web/dist /src/web/dist
RUN touch src/main.rs src/lib.rs && cargo build --release --locked

# --- Docker Compose CLI (standalone; tracked by Dependabot) ----------------
FROM docker/compose-bin:v5.7.0 AS compose

# --- Bare binary, for release downloads (`--target binary`) ---------------
FROM scratch AS binary
COPY --from=server /src/server/target/release/dinghy /dinghy

# --- Final image ------------------------------------------------------------
FROM gcr.io/distroless/static-debian13:nonroot
LABEL org.opencontainers.image.title="dinghy" \
      org.opencontainers.image.description="A lightweight Docker Compose stack manager for a single server" \
      org.opencontainers.image.source="https://github.com/Joaodss/dinghy"
COPY --from=compose /docker-compose /usr/local/bin/docker-compose
COPY --from=server /src/server/target/release/dinghy /usr/local/bin/dinghy
ENV DINGHY_LISTEN=0.0.0.0:5002 \
    DINGHY_STACKS_DIR=/opt/stacks \
    DINGHY_COMPOSE_CMD=/usr/local/bin/docker-compose \
    HOME=/tmp
EXPOSE 5002
USER nonroot
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s CMD ["/usr/local/bin/dinghy", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/dinghy"]

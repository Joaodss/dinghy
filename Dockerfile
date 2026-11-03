# syntax=docker/dockerfile:1

# --- Frontend -------------------------------------------------------------
FROM node:24-alpine AS web
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

# --- Docker Compose CLI (standalone binary, no docker CLI needed) ---------
FROM alpine:3.22 AS compose
ARG COMPOSE_VERSION=v2.39.4
ARG TARGETARCH
RUN apk add --no-cache curl \
    && case "${TARGETARCH:-amd64}" in amd64) arch=x86_64 ;; arm64) arch=aarch64 ;; *) echo "unsupported arch" && exit 1 ;; esac \
    && cd /tmp \
    && curl -fsSLO "https://github.com/docker/compose/releases/download/${COMPOSE_VERSION}/docker-compose-linux-${arch}" \
    && curl -fsSL "https://github.com/docker/compose/releases/download/${COMPOSE_VERSION}/docker-compose-linux-${arch}.sha256" | sha256sum -c - \
    && install -m 0755 "docker-compose-linux-${arch}" /docker-compose

# --- Final image ------------------------------------------------------------
FROM gcr.io/distroless/static-debian12:nonroot
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

# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-11-03

First release.

### Added

- Stack list with aggregate status (running, partial, stopped, error),
  running containers and memory, plus containers that belong to no stack.
- Stack detail with Up, Down, Restart and Pull + Up, running
  `docker compose` and streaming its output.
- Live container logs over Server-Sent Events, with filter and pause in the
  browser. The server holds no log buffer and closes the Docker stream when
  the page closes.
- File editor for configured roots: atomic saves refused with `412` when the
  file changed on disk, masked `.env` values, create and rename.
- Three gates: Cloudflare Access JWT, an argon2id password with a signed
  12-hour session cookie, and a CSRF token on state-changing requests.
- `dinghy hash-password` and `dinghy healthcheck` commands.
- Distroless, non-root image (about 43 MB) with the Docker Compose v5 CLI,
  for `linux/amd64` and `linux/arm64`.
- Precompressed (Brotli and gzip) web assets embedded in the binary.

[Unreleased]: https://github.com/Joaodss/dinghy/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Joaodss/dinghy/releases/tag/v0.1.0

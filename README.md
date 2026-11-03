# dinghy

A small Docker Compose stack manager for one server. It replaces Dockge on a
963 MB e2-micro, where Dockge alone used 213 MB of RAM.

dinghy keeps no database, cache or background jobs. It reads stacks from disk
and state from the Docker API on each request, and runs `docker compose` only
while an action is running. Logs stream only while their page is open.

## What it does

- **Stacks**: one row per folder in the stacks dir that has `compose.yaml`,
  `compose.yml`, `docker-compose.yml` or `docker-compose.yaml`. It shows the
  aggregate status (running, partial, stopped, error), the number of running
  containers and the memory they use. Containers that belong to no stack are
  listed apart.
- **Stack detail**: each container's image, state, health, uptime, memory and
  limit, and ports. **Up**, **Down**, **Restart** and **Pull + Up** run
  `docker compose -p <stack> -f <file> …` and stream its output; the output
  is dropped two minutes after the command ends.
- **Logs**: the last 200 lines, then follow, over Server-Sent Events. The
  filter and pause run in the browser, which keeps the newest 5,000 lines.
  Closing the page closes the Docker log stream. Nothing is buffered on the
  server: Docker already stores the logs, and `tail` reads them on demand.
  Busy containers stay smooth: the server groups waiting lines into one
  event, and the page draws at most once per frame, skipping off-screen
  lines.
- **Files**: a tree of the configured roots and a CodeMirror editor. Saves are
  atomic (temp file + rename) and refused with `412` if the file changed since
  it was opened. `.env` values are masked until you press **Show values**, and
  the masked view is read-only. Files over 1 MB or binary files do not open.
  You can create and rename files and folders; deleting is not in v1.

Non-goals for v1: multiple servers, users or roles, a terminal into containers,
image builds, metrics history.

## Security model

Anyone who gets into dinghy has root on the host in practice (the Docker socket
plus file writes). There are three gates, and none is optional:

1. **Network**: publish the port on `127.0.0.1` only and put Caddy in front.
   Public traffic arrives only through Cloudflare.
2. **Identity**: Cloudflare Access protects the subdomain. dinghy checks the
   `Cf-Access-Jwt-Assertion` header (or `CF_Authorization` cookie) on every
   request except `/healthz`: RS256 signature against the team's public keys,
   `aud`, `iss`, expiry, and an allow-listed email.
3. **Application**: a password (argon2id hash in an env var) gives a
   12-hour session cookie (`HttpOnly`, `Secure`, `SameSite=Strict`). Every
   `POST`/`PUT` also needs the session's CSRF token in `X-Dinghy-CSRF`.

In the code:

- `docker compose` runs with an argument list, never `sh -c`, and with a
  cleared environment, so dinghy's own secrets never reach it.
- Stack names must match `^[a-z0-9][a-z0-9_-]*$` and name an existing folder
  before anything runs. Container ids are checked too.
- File paths are rejected if they contain `..`, are absolute or contain `\`,
  `:` or NUL; they are then canonicalized and must stay inside their root, so
  symlinks that leave the root are refused. Roots are names from the config,
  never paths.
- `.env` contents are never logged, and are only returned unmasked when the
  request asks for it.
- The image is distroless, runs as `nonroot` (uid 65532), and is meant to run
  with `--read-only --cap-drop ALL --security-opt no-new-privileges`.

## Configuration

All settings are environment variables.

| Variable | Default | Purpose |
| --- | --- | --- |
| `DINGHY_LISTEN` | `127.0.0.1:5002` (image: `0.0.0.0:5002`) | Listen address |
| `DINGHY_STACKS_DIR` | `/opt/stacks` | Stacks root |
| `DINGHY_STACKS_HOST_DIR` | unset | Stacks root as the Docker host sees it, when it differs (see below) |
| `DINGHY_FILE_ROOTS` | `stacks=<DINGHY_STACKS_DIR>` | Editor roots, e.g. `stacks=/opt/stacks,scripts=/opt/scripts` |
| `DINGHY_PASSWORD_HASH` | required | argon2id PHC string from `dinghy hash-password` |
| `DINGHY_CF_TEAM` | required | Cloudflare Access team (`noodleserver`) or full team domain |
| `DINGHY_CF_AUD` | required | Access application audience tag |
| `DINGHY_ALLOWED_EMAILS` | required | Comma-separated emails allowed in |
| `DINGHY_INSECURE_NO_ACCESS` | unset | `1` skips the Access check. Local development only |
| `DINGHY_SESSION_SECRET` | random per start | 64+ hex chars, to keep sessions across restarts |
| `DINGHY_COOKIE_SECURE` | `true` | `false` only for plain-HTTP development off localhost |
| `DINGHY_COMPOSE_CMD` | `docker compose` (image: `/usr/local/bin/docker-compose`) | How Compose is invoked |
| `DINGHY_LOG` | `info` | `debug`, `info`, `warn` or `error` |

Create the password hash (it reads one line from stdin):

```bash
printf '%s' 'your password' | docker run --rm -i ghcr.io/joaodss/dinghy:0.1.0 hash-password
```

In an env file read by `docker run --env-file` or Compose `env_file:`, write
the hash as is; those formats do not expand `$`.

## Install

Images are on GitHub Container Registry, for `linux/amd64` and `linux/arm64`,
with SBOM and provenance attestations:

| Tag | Points to |
| --- | --- |
| `0.1.0` | That exact release. Pin this in production. |
| `0.1` | The newest `0.1.x` patch release. |
| `latest` | The newest release. |
| `edge` | The tip of `master`. Not for production. |
| `sha-<commit>` | One specific `master` commit. |

Each [release](https://github.com/Joaodss/dinghy/releases) also ships static
binaries (`dinghy-x86_64-linux-musl`, `dinghy-aarch64-linux-musl`) with the
web UI embedded, plus `SHA256SUMS`. They need `docker compose` on the host.

The [`deploy/`](deploy/) folder has ready-to-use files:

- [`compose.yaml`](deploy/compose.yaml): run dinghy with Docker Compose.
- [`dinghy.service`](deploy/dinghy.service): a systemd unit around `docker run`,
  for hosts without the Compose plugin, such as Container-Optimized OS.
- [`dinghy.env.example`](deploy/dinghy.env.example): the secrets to fill in.

### With Docker Compose

```bash
cd deploy
cp dinghy.env.example dinghy.env        # fill it in
cat > .env <<EOF
DOCKER_GID=$(stat -c %g /var/run/docker.sock)
STACKS_DIR=/mnt/disks/data/stacks
SCRIPTS_DIR=/mnt/disks/data/scripts
EOF
docker compose up -d
```

Keep `deploy/` outside the stacks directory, so dinghy does not list itself
and cannot stop itself with **Down**. To upgrade, change `DINGHY_VERSION` in
`.env` (or the tag in `compose.yaml`) and run `docker compose up -d` again.

## Deploying on noodle-server

Container-Optimized OS has no Compose plugin, so use
[`deploy/dinghy.service`](deploy/dinghy.service) in place of `dockge.service`
in the Terraform cloud-init (`locals.tf`). It pulls the pinned image, runs it
hardened and restarts it if it stops. Set the socket's group id in
`--group-add`. systemd does not run `$(…)`, so write the number from
`stat -c %g /var/run/docker.sock`.

Bind mounts in compose files resolve against the project directory, and the
Docker daemon reads those paths on the host. Both deploy files therefore
mount the stacks at the **same path** inside the container.

If you keep the Dockge layout (`-v /mnt/disks/data/stacks:/opt/stacks`), set
`DINGHY_STACKS_HOST_DIR=/mnt/disks/data/stacks`. dinghy then passes
`--project-directory <host path>` and `--env-file <stack>/.env` to Compose.
That covers bind mounts and `.env`, but not `env_file:` entries or `build:`
contexts with relative paths, which Compose also reads from the project
directory.

Permissions, because the process is not root:

- `--group-add` gives it the Docker socket's group.
- To save files it must be able to write to the folder that holds them (the
  save is a rename). Give uid 65532 write access to the stack folders only,
  not recursively, so data folders inside stacks keep their owners:
  `chown 65532 /mnt/disks/data/stacks /mnt/disks/data/stacks/*/ /mnt/disks/data/scripts`.

### Migrating from Dockge

1. Run dinghy next to Dockge on another port and compare memory with
   `docker stats --no-stream dinghy dockge`.
2. Point the Caddy subdomain at dinghy.
3. Swap `dockge.service` for `dinghy.service` in Terraform and apply.
4. Delete `/mnt/disks/data/dockge` after a week without problems.

## Performance targets

Measured for 0.1.0 in the hardened container on Docker Desktop (Linux VM):

| Metric | Target | Measured | How to measure |
| --- | --- | --- | --- |
| RAM at rest, no page open | ≤ 15 MB | 1.1 MiB | `docker stats --no-stream dinghy`, 10 min after start |
| RAM with log streams open | ≤ 40 MB | ~1 MiB (3 streams) | same, with a container's logs open |
| RAM after closing the page | rest + 2 MB | 0.9 MiB | same, after closing the browser |
| CPU at rest | ~0% | 0.00% | no polling without clients |
| Image | ≤ 80 MB | 43 MB | `docker images dinghy` |
| Binary | ≤ 15 MB | 6 MB | `ls -l` of the release binary |
| Start-up | ≤ 1 s | ~0.9 s | time until `/healthz` answers `200` |

## HTTP API

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/api/session` | `{authenticated, csrf, email}` |
| `POST` | `/api/login` | `{password}` → session cookie + CSRF token |
| `POST` | `/api/logout` | Clears the cookie |
| `GET` | `/api/stacks` | Stacks with status, plus containers without a stack |
| `GET` | `/api/stacks/{stack}` | Containers of one stack |
| `POST` | `/api/stacks/{stack}/{up\|down\|restart\|pull}` | Starts an action, returns `{op}` |
| `GET` | `/api/ops/{id}/stream` | SSE: `lines` events, then `done` with the exit code |
| `GET` | `/api/containers/{id}/logs?tail=200` | SSE: `lines` events until the client leaves |
| `GET` | `/api/roots` | Editor roots |
| `GET` | `/api/files?root=…&path=…` | Directory listing |
| `GET` | `/api/files/content?root=…&path=…[&reveal=true]` | File content, with `ETag` |
| `PUT` | `/api/files/content?root=…&path=…` | Save; needs `If-Match` (`428` without, `412` if stale) |
| `POST` | `/api/files/create` | `{root, path, dir}` |
| `POST` | `/api/files/rename` | `{root, from, to}` |
| `GET` | `/healthz` | `200` when the Docker socket answers |

## Development

```
server/   Rust: axum + tokio (single-threaded runtime), bollard, rust-embed
web/      Preact + Vite; CodeMirror 6 loads only on the Files page
```

```bash
cd web && npm ci && npm run build      # also writes .br/.gz copies; the server embeds web/dist
cd server && cargo test
```

Run locally against your Docker daemon:

```bash
cd server
export DINGHY_PASSWORD_HASH="$(printf 'dev' | cargo run -q -- hash-password)"
DINGHY_INSECURE_NO_ACCESS=1 DINGHY_STACKS_DIR=/path/to/stacks cargo run
```

For frontend work, `npm run dev` in `web/` proxies `/api` to
`127.0.0.1:5002`.

To check a built image end to end (health, assets, sign-in, compose up and
down, memory), as CI does:

```bash
docker build -t dinghy:dev . && scripts/smoke-test.sh dinghy:dev
```

## CI/CD and releases

[`ci.yml`](.github/workflows/ci.yml) runs on every pull request and push:

- **Web**: `npm ci`, `npm audit`, build.
- **Server**: `cargo fmt`, `clippy -D warnings`, tests, `cargo audit`.
- **Image**: build, [smoke test](scripts/smoke-test.sh), and a Trivy scan
  that fails on fixable HIGH or CRITICAL vulnerabilities.

Pushes to `master` then publish `edge` and `sha-*` images. Tags publish the
release images and a GitHub release.
[`security.yml`](.github/workflows/security.yml) repeats the audits and scans
the published images every Monday, because advisories appear without code
changes. [Dependabot](.github/dependabot.yml) opens weekly PRs for Rust and
npm packages, the Dockerfile base images (including the Compose CLI) and the
workflow actions.

To release:

1. Set the new version in `server/Cargo.toml` (run `cargo check` to update
   `Cargo.lock`) and `web/package.json`.
2. Move the `Unreleased` notes in `CHANGELOG.md` under a new
   `## [X.Y.Z] - date` heading.
3. Commit, then tag and push: `git tag vX.Y.Z && git push origin vX.Y.Z`.

CI refuses a tag that doesn't match both versions or has no changelog
section. Tags with a suffix (`v1.0.0-rc.1`) become pre-releases and don't
move `latest`.

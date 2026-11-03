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
printf '%s' 'your password' | docker run --rm -i ghcr.io/joaodss/dinghy:latest hash-password
```

In an env file read by `docker run --env-file`, write the hash as is; that
format does not expand `$`.

## Deploying on noodle-server

Bind mounts in compose files resolve against the project directory, and the
Docker daemon reads those paths on the host. The simplest fix is to mount the
stacks at the **same path** inside the container:

```bash
docker run --rm --name dinghy \
  --network custom-bridge \
  --read-only --tmpfs /tmp \
  --cap-drop ALL --security-opt no-new-privileges \
  --memory 64m \
  --group-add "$(stat -c %g /var/run/docker.sock)" \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v /mnt/disks/data/stacks:/mnt/disks/data/stacks \
  -v /mnt/disks/data/scripts:/mnt/disks/data/scripts \
  -e DINGHY_STACKS_DIR=/mnt/disks/data/stacks \
  -e DINGHY_FILE_ROOTS=stacks=/mnt/disks/data/stacks,scripts=/mnt/disks/data/scripts \
  --env-file /mnt/disks/data/dinghy/dinghy.env \
  -p 127.0.0.1:5002:5002 \
  ghcr.io/joaodss/dinghy:latest
```

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

`dinghy.env` holds the secrets:

```
DINGHY_PASSWORD_HASH=$argon2id$v=19$m=19456,t=2,p=1$...
DINGHY_CF_TEAM=noodleserver
DINGHY_CF_AUD=<aud tag>
DINGHY_ALLOWED_EMAILS=you@example.com
DINGHY_SESSION_SECRET=<openssl rand -hex 32>
```

For the systemd unit in the Terraform cloud-init (`locals.tf`), replace
`dockge.service` with a `dinghy.service` whose `ExecStart` is the command
above, plus `ExecStartPre=-/usr/bin/docker rm -f dinghy` and `Restart=always`.
systemd does not run `$(…)`, so write the socket's numeric group id in
`--group-add` there.

### Migrating from Dockge

1. Run dinghy next to Dockge on another port and compare memory with
   `docker stats --no-stream dinghy dockge`.
2. Point the Caddy subdomain at dinghy.
3. Swap `dockge.service` for `dinghy.service` in Terraform and apply.
4. Delete `/mnt/disks/data/dockge` after a week without problems.

## Performance targets

| Metric | Target | How to measure |
| --- | --- | --- |
| RAM at rest, no page open | ≤ 15 MB | `docker stats --no-stream dinghy`, 10 min after start |
| RAM with one log stream open | ≤ 40 MB | same, with a container's logs open |
| RAM 1 min after closing the page | back to rest + 2 MB | same, after closing the browser |
| CPU at rest | ~0% | no polling without clients |
| Image | ≤ 80 MB | `docker images dinghy` |
| Binary | ≤ 15 MB | `ls -l` of the release binary |
| Start-up | ≤ 1 s | time until `/healthz` answers `200` |

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

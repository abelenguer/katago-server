# Deployment

## Docker images

Images built from the main `Dockerfile` are published to
`ghcr.io/goban-app/katago-server`. Each release `X.Y.Z` produces
`X.Y.Z-<variant>` and `latest-<variant>`; the plain `X.Y.Z` and `latest` tags
are the CPU variant.

| Variant | KataGo | Networks | Platforms | Notes |
|---|---|---|---|---|
| `cpu` (= `latest`) | 1.18.2, Eigen backend | `kata1-b28c512nbt-s12043015936-d5616446734.bin.gz` | amd64, arm64 | Default. |
| `human-cpu` | Eigen | `b18c384nbt-humanv0.bin.gz` only | amd64, arm64 | Human-style analysis only. |
| `combo-cpu` | Eigen | b28 standard + human network | amd64, arm64 | Human SL via `overrideSettings.humanSLProfile`. |
| `gpu` | CUDA 12.4 + cuDNN | b28 | amd64 | Needs NVIDIA Container Toolkit, driver >= 525.60. |
| `human-gpu` | CUDA | human network only | amd64 | |
| `combo-gpu` | CUDA | b28 + human | amd64 | |
| `minimal` | none | none | amd64, arm64 | Mount `/models/katago`, `/models/model.bin.gz`, `/models/analysis_config.cfg`. |
| `base` | none | none | amd64, arm64 | Server binary only (statically linked); set all `KATAGO_*` paths yourself. |

These images run as UID 1000 with `WORKDIR /app`, expose port 2718, log at `info`,
and define a `HEALTHCHECK` that runs `katago-server healthcheck` (a built-in HTTP
probe of `/api/v1/health`, no curl or wget needed). KataGo is built from source
at v1.18.2.

```bash
# GPU
docker run --gpus all -p 2718:2718 ghcr.io/goban-app/katago-server:latest-gpu

# custom network and KataGo config on top of the CPU image
docker run -p 2718:2718 \
  -v "$PWD/my-net.bin.gz:/models/my-net.bin.gz:ro" \
  -v "$PWD/analysis_config.cfg:/models/analysis_config.cfg:ro" \
  -e KATAGO_MODEL_PATH=/models/my-net.bin.gz \
  -e KATAGO_CONFIG_PATH=/models/analysis_config.cfg \
  ghcr.io/goban-app/katago-server:latest

# bring your own KataGo binary
docker run -p 2718:2718 -v /path/to/models:/models:ro ghcr.io/goban-app/katago-server:latest-minimal
```

Mounted binaries must be executable, models/configs readable, and parent
directories searchable by UID/GID 1000:1000. Do not run as root to bypass mount
permissions. See [configuration.md](configuration.md#container-and-helm-overrides)
for `minimal` path overrides, `combo-*` human-network paths and environment
precedence when replacing `config.toml`.

Build locally with `docker build --target <variant> -t katago-server:<variant> .`.
Build arguments: `KATAGO_VERSION` (git tag, default `v1.18.2`), `STANDARD_MODEL`
and `HUMAN_MODEL` (network file names), `STANDARD_MODEL_SHA256` and
`HUMAN_MODEL_SHA256` (verify the downloads when set), `CUDA_VERSION`, `RUST_VERSION`.

### Complete CUDA 12.8 image

[`Dockerfile.katago-cuda`](KATAGO_CUDA_IMAGE.md) is a separate complete server
image using the official KataGo 1.18.0 CUDA 12.8/cuDNN 9.8.0 release, a static
Rust server built from the checkout, a pinned b28 model, and dedicated configs.
It starts `/app/katago-server serve` as UID 1000 on `0.0.0.0:2718` with JSON logs;
no runtime downloads or mounts are needed, and no HumanSL model is bundled.

Port precedence is `KATAGO_SERVER_PORT` > `PORT` > TOML `server.port`.
`KATAGO_CONFIG_FILE=/app/config.toml` selects the server TOML for both serving
and health checks; `KATAGO_CONFIG_PATH` selects the engine config instead.
The healthcheck probes `/api/v1/health` with a 300-second start period;
readiness and liveness use `/api/v1/health/ready` and `/api/v1/health/live`.
See the [image guide](KATAGO_CUDA_IMAGE.md) for local GPU runs, optional tuning,
non-GPU checks, and its unchanged GHCR publishing interface. The main
`Dockerfile` versions, variants, and build contracts above remain unchanged.

## Docker Compose

`docker-compose.yml` in the repository starts the CPU image with a health check
and contains commented GPU and minimal services. Run `docker compose up -d`.

## Helm

```bash
helm repo add katago-server https://goban-app.github.io/katago-server
helm repo update
helm install katago katago-server/katago-server --version 1.8.0
```

Useful values (see `charts/katago-server/values.yaml` for all):

| Value | Purpose |
|---|---|
| `image.variant` | `""` (cpu), `-gpu`, `-combo-cpu`, `-minimal`, ... |
| `resources` | CPU images want 2 to 4 cores and 1 to 2 GiB; GPU images 2 to 4 GiB plus one GPU. |
| `gpu.enabled`, `gpu.count`, `gpu.vendor` | Requests `nvidia.com/gpu` (or `amd.com/gpu`). |
| `config.logLevel`, `config.logFormat` | `RUST_LOG` and `KATAGO_SERVER_LOG_FORMAT`. |
| `config.katago.moveTimeoutSecs`, `config.katago.analysisConfig` | Engine timeout and the mounted `analysis_config.cfg`. |
| `config.customConfig` | A full `config.toml`, mounted and selected via `KATAGO_CONFIG_FILE`. |
| `config.customModel.*` | Init container that downloads a network (optionally checksum-verified) before start. |
| `autoscaling`, `podDisruptionBudget`, `ingress`, `serviceMonitor` | Standard extras. `serviceMonitor` scrapes `/metrics`. |

Probes: liveness on `/api/v1/health/live`, readiness and startup on
`/api/v1/health/ready`. The startup probe allows several minutes because loading
a large network on CPU is slow.

`config.customConfig` does not override environment variables emitted by the
chart or image; review the [override rules](configuration.md#container-and-helm-overrides),
especially when using `minimal` or replacing a `combo-*` configuration.

## systemd

`katago-server.service` runs `/usr/local/bin/katago-server` with
`KATAGO_CONFIG_FILE=/opt/katago-server/config.toml`, restarts on failure, and
stops with SIGTERM. `make install` copies the binary and unit; then run
`systemctl enable --now katago-server`.

Create the non-root `katago` user/group first. Give it read access to models and
configs, execute access to KataGo, and search permission on parent directories.
Keep these files outside home directories hidden by `ProtectHome=true`.

The base unit deliberately has `PrivateDevices=true`, which hides physical GPU
devices. For a GPU installation, use `sudo systemctl edit katago-server` to add
a deployment-specific drop-in, not a change to the shipped unit:

```ini
[Service]
PrivateDevices=false
```

Grant the service user access to the actual GPU device nodes (`/dev/nvidia*` for
CUDA or `/dev/dri/renderD*` for OpenCL). For example, add
`SupplementaryGroups=render video` to the drop-in only if those groups exist and
own the required devices on your host. If another policy sets `DevicePolicy`,
also allow the required devices there; `DeviceAllow` alone cannot expose devices
hidden by `PrivateDevices=true`. Keep the other hardening settings. Apply with
`sudo systemctl daemon-reload` and `sudo systemctl restart katago-server`.

### OpenCL / Rusticl profile

OpenCL is an independent, bring-your-own-binary profile, not a switch that turns
a CUDA binary or the shipped CUDA images into an OpenCL build. Point
`katago.katago_path` at an OpenCL KataGo executable and install the matching
OpenCL ICD/runtime and host driver. For Mesa Rusticl on a supported AMD setup,
set `RUSTICL_ENABLE=radeonsi` in the process environment (or
`Environment=RUSTICL_ENABLE=radeonsi` in the GPU service drop-in). Other hardware
may need a different driver selection.

Verify device access as the same non-root user that runs the service. Give
KataGo tuning files and OpenCL runtime caches writable locations under
`/opt/katago-server`, owned by `katago`, configuring their locations separately.
Do not disable filesystem hardening globally. CUDA and OpenCL configuration and
runtime dependencies should remain separate.

## Reverse proxy

The server has no authentication or TLS. Protect it with platform authentication
and TLS or a reverse proxy providing both. Restrict network access and
`server.cors_allowed_origins`, and give the proxy a read timeout at least as long
as `server.request_timeout_secs` (300 s by default) so long game analyses are
not cut off.

For WS, explicitly forward the upgrade headers. For example, inside an nginx
`server` block serving your TLS endpoint:

```nginx
location = /api/v1/analysis/ws {
    proxy_pass http://127.0.0.1:2718;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_set_header Host $host;
    proxy_read_timeout 300s;
}
```

The proxy read timeout is an inactivity limit, not the WS query deadline or
socket lifetime. Set it above the expected quiet analysis interval, and have
clients periodically send `{"type":"ping"}` while idle so replies keep the
connection alive. Configure equivalent upgrade and idle-timeout behavior on
ingress/load balancers. The server checks supplied WS `Origin` against the CORS
origin list; clients without `Origin` are allowed, so this is not access control.

## Shutdown

On SIGTERM or SIGINT the HTTP listener stops accepting connections and in-flight
HTTP requests finish (bounded by the request timeout). The server then sends KataGo
`terminate_all`, closes its stdin, waits up to 5 s, and kills it if needed. New
requests during shutdown get 503 `shutting-down`. Set Kubernetes
`terminationGracePeriodSeconds` above your request timeout with room for WS
cleanup if every HTTP request must finish.

Shutdown tracks upgraded WS sessions separately from HTTP requests and waits a
bounded time for their cleanup. Pending WS queries are cancelled on disconnect
or engine shutdown. Server shutdown attempts to send `shutting_down` followed
by `completed` with status `error`, but delivery is best-effort within that
bounded wait. Clients must fail pending work when a socket closes and resubmit
explicitly after reconnect; there is no resume/replay. After an engine restart,
sockets that remain open can accept new queries once earlier queries have
received terminal errors.

## Observability

Prometheus metrics at `/metrics`:

| Metric | Type | Labels |
|---|---|---|
| `http_requests_total` | counter | `method`, `route`, `status` |
| `http_request_duration_seconds` | histogram | `method`, `route` |
| `http_requests_in_flight` | gauge | |
| `katago_analysis_requests_total` | counter | `outcome` = `ok`, `timeout`, `rejected`, `unavailable`, `error` |
| `katago_analysis_duration_seconds` | histogram | `outcome` |
| `katago_ws_requests_total` | counter | `outcome` = `ok`, `cancelled`, `timeout`, `error` |
| `katago_ws_duration_seconds` | histogram | `outcome` |
| `katago_engine_up` | gauge | 1 while KataGo runs |
| `katago_engine_restarts_total` | counter | |

The WS metrics record terminal completion handling, including explicit cancels
and execution failures before `accepted`. They exclude validation/control
rejections and queries aborted by socket loss without reaching completion.
Durations run from request registration to completion handling; neither metric
confirms that the client received the terminal message.

Logs are text by default or JSON with `KATAGO_SERVER_LOG_FORMAT=json`;
`Dockerfile.katago-cuda` defaults to JSON. Each request is traced under an
`http` span carrying `method`, `path` and
`request_id`; the `x-request-id` response header holds the same id (yours if you
sent one).

## Sizing

- One KataGo process per server instance. Scale horizontally with more replicas rather than huge thread counts.
- CPU: the b28 network takes seconds per position at 50 visits on a few cores. Prefer the smaller b18 network for latency-sensitive CPU deployments.
- GPU: a single mid-range NVIDIA GPU handles hundreds of visits per second; raise `numAnalysisThreads`, `nnMaxBatchSize` and `max_concurrent_requests` accordingly.
- Set `katago.max_visits_limit` on public deployments so a single request cannot monopolise the engine.

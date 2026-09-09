# Configuration

Settings are resolved in this order, later sources winning:

1. built-in defaults
2. `config.toml`: the file given by `--config FILE` or `KATAGO_CONFIG_FILE`, else `./config.toml` if it exists
3. environment variables

For the TCP port, precedence is `KATAGO_SERVER_PORT` > platform `PORT` > TOML/default.
`PORT` is ignored entirely when `KATAGO_SERVER_PORT` is present, even if `PORT` is
malformed. This applies equally to `serve`, `check-config`, and `healthcheck`.

An unparseable selected environment override is an error, not ignored. Unknown
keys in `config.toml` are rejected. `katago-server check-config` loads, validates
(including that the binary, network and KataGo config exist) and prints the
effective configuration as TOML.

## `[server]`

| Key | Type | Default | Env var | Description |
|---|---|---|---|---|
| `host` | string | `"::"` | `KATAGO_SERVER_HOST` | Bind address. `::` serves IPv6 and IPv4 on most systems. |
| `port` | integer | `2718` | `KATAGO_SERVER_PORT`, fallback `PORT` | TCP port. |
| `request_timeout_secs` | integer | `300` | `KATAGO_SERVER_REQUEST_TIMEOUT_SECS` | Upper bound on any single HTTP request, not an upgraded WS connection's lifetime. Must be at least `katago.move_timeout_secs`. |
| `max_concurrent_requests` | integer | `256` | `KATAGO_SERVER_MAX_CONCURRENT_REQUESTS` | In-flight requests before load shedding with 503. |
| `max_body_bytes` | integer | `1048576` | `KATAGO_SERVER_MAX_BODY_BYTES` | Maximum HTTP request body and each incoming WS frame/message. At least 1024. |
| `cors_allowed_origins` | list of strings | `["*"]` | `KATAGO_SERVER_CORS_ALLOWED_ORIGINS` (comma-separated) | `*` allows any origin; otherwise an explicit list. Must not be empty. Also checked against supplied WS `Origin`; CLI clients without `Origin` are allowed. |
| `log_format` | `text` or `json` | `text` | `KATAGO_SERVER_LOG_FORMAT` | JSON emits one object per line for log aggregators. |

## `[katago]`

| Key | Type | Default | Env var | Description |
|---|---|---|---|---|
| `katago_path` | path | `./katago` | `KATAGO_KATAGO_PATH` | The `katago` executable. |
| `model_path` | path | `./model.bin.gz` | `KATAGO_MODEL_PATH` | Main neural network. |
| `human_model_path` | path | unset | `KATAGO_HUMAN_MODEL_PATH` | Human SL network; enables `humanSLProfile` overrides. Empty string unsets. |
| `config_path` | path | `./analysis_config.cfg` | `KATAGO_CONFIG_PATH` | KataGo analysis engine `.cfg`. |
| `move_timeout_secs` | integer | `20` | `KATAGO_MOVE_TIMEOUT_SECS` | REST: silence tolerated per analysed turn before termination and 504. WS: total query deadline, returning `timeout` and terminal `completed`. |
| `default_max_visits` | integer | `10` | `KATAGO_DEFAULT_MAX_VISITS` | `maxVisits` when the request omits it. `0` or empty unsets it, leaving the `.cfg` value in charge. |
| `max_visits_limit` | integer | unset | `KATAGO_MAX_VISITS_LIMIT` | Requests (including `overrideSettings.maxVisits`) above this get 400. |
| `max_restart_attempts` | integer | `10` | `KATAGO_MAX_RESTART_ATTEMPTS` | Restarts after KataGo exits before the server gives up and liveness fails. |

Validation rules: `port` > 0, timeouts > 0, `request_timeout_secs >= move_timeout_secs`,
`default_max_visits <= max_visits_limit` when both are set.

WS allows 32 active queries per connection independently of the HTTP concurrency
limit. Its bounded outgoing queue and write timeout disconnect slow readers;
see the [WS protocol](api.md#get-apiv1analysisws).

## Container and Helm overrides

The same environment-over-file precedence applies inside containers:

- `minimal` sets `KATAGO_KATAGO_PATH`, `KATAGO_MODEL_PATH` and `KATAGO_CONFIG_PATH` to `/models/katago`, `/models/model.bin.gz` and `/models/analysis_config.cfg`. Override those environment variables when using different paths; a mounted `config.toml` alone cannot override them. Do not mount over `/app` and hide the server binary.
- Bundled images built from the main `Dockerfile` use a build-time generated `/app/config.toml`. `combo-*` records both `model_path` and `human_model_path` there. Replacing that file must retain both paths, or supply `KATAGO_MODEL_PATH` and `KATAGO_HUMAN_MODEL_PATH` explicitly. `KATAGO_MODEL` and `KATAGO_HUMAN_MODEL` are build-time download inputs, not runtime path overrides.
- `Dockerfile.katago-cuda` instead copies `config.toml.cuda` to `/app/config.toml` and `analysis_config.cfg.cuda` to `/app/analysis_config.cfg`, and bundles the standard model at `/app/model.bin.gz`. It binds `0.0.0.0:2718` by default and honors platform `PORT`; see [the CUDA image guide](KATAGO_CUDA_IMAGE.md) for its image-specific defaults.
- Helm `config.customConfig` selects `/config/config.toml` with `KATAGO_CONFIG_FILE`; it does not disable chart-generated or image environment variables. In particular, chart `service.targetPort` and `config.katago.moveTimeoutSecs` still win. For `minimal`, align the `/models` mounts and path variables; the chart's `analysisConfig` mounts at `/app/analysis_config.cfg`, so set `config.katago.configPath` to use that file. For `combo-*`, include both network paths in a custom TOML or set the chart's `modelPath` and `humanModelPath`.

Use `katago-server check-config` in the actual container/service environment to
check the effective paths and values.

## Logging

`RUST_LOG` filters output (default `info,katago_server=info,tower_http=info`).
`RUST_LOG=debug` also shows every line exchanged with KataGo under the
`katago::stdin`, `katago::stdout` and `katago::stderr` targets. When KataGo exits
unexpectedly the last 40 stderr lines are logged at error level.

## Example `config.toml`

```toml
[server]
host = "::"
port = 2718
cors_allowed_origins = ["https://goban.app"]
log_format = "json"

[katago]
katago_path = "/usr/local/bin/katago"
model_path = "/models/kata1-b28c512nbt-s12043015936-d5616446734.bin.gz"
config_path = "/etc/katago/analysis_config.cfg"
move_timeout_secs = 30
default_max_visits = 50
max_visits_limit = 2000
```

## Tuning the KataGo analysis config

The server starts `katago analysis -model ... -config <config_path>`. The shipped
`analysis_config.cfg.*` files are starting points:

| Setting | CPU | GPU | Meaning |
|---|---|---|---|
| `numAnalysisThreads` | 1 to 2 | 4 to 8 | Positions searched in parallel. Game analysis benefits directly. |
| `numSearchThreadsPerAnalysisThread` | 1 to 2 | 4 to 8 | Threads per position. |
| `nnMaxBatchSize` | 8 | 32 to 64 | Neural network batch size. |
| `numNNServerThreadsPerModel` | 1 to 2 | 1 to 2 | One per GPU is typical. |
| `maxVisits` | 10 to 100 | 200 to 2000 | Default visits when neither the request nor `default_max_visits` sets one. |
| `nnCacheSizePowerOfTwo` | 18 to 20 | 20 to 23 | Cache entries as a power of two; memory grows accordingly. |

The built-in API fallback is `default_max_visits = 10`; the dedicated CUDA image
sets it to 50 in its bundled TOML. Even an active
`maxVisits = 1000` in the KataGo `.cfg` does not force 1000 visits when the API
supplies 10. Request a different `maxVisits` explicitly, change the server
fallback, or unset it with `KATAGO_DEFAULT_MAX_VISITS=0` to defer to the `.cfg`.

Keep `reportAnalysisWinratesAs = SIDETOMOVE` so the API semantics hold. The
shipped configs set `logToStderr = true` and no `logDir`, so KataGo writes no log
files; the server captures stderr instead. Keep `move_timeout_secs` above the
time a single position takes at your `maxVisits`; for WS, budget for the entire
set of requested turns instead.

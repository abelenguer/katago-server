# KataGo CUDA Image

`Dockerfile.katago-cuda` builds a complete `linux/amd64` CUDA HTTP API image:
`katago-server` built from this checkout, the official KataGo 1.18.0 CUDA
12.8/cuDNN 9.8.0 executable, a pinned neural-network model, and dedicated server
and analysis configurations. No runtime downloads or mounts are required.
No HumanSL model is bundled.

The main `Dockerfile` and its other CPU/CUDA versions, variants, and build
contracts remain unchanged; see [deployment.md](deployment.md#docker-images).

**Default-command change:** this image now starts `/app/katago-server serve`,
not `katago version`. For the old version-only check, explicitly use
`--entrypoint /usr/local/bin/katago` and pass `version`, as shown below.

## Image Contents

The runtime base is `nvidia/cuda:12.8.1-cudnn-runtime-ubuntu22.04`. The image runs
as non-root UID/GID `1000:1000`, with `WORKDIR /app` and `HOME=/home/katago`.
Its entrypoint is `/app/katago-server`, its `CMD` is `serve`, and it exposes
port `2718`, matching the core's default connection port.

| Image path | Source |
| --- | --- |
| `/app/katago-server` | Static musl Rust server built from this checkout using `rust:1.92-slim`. |
| `/usr/local/bin/katago` | Official [KataGo 1.18.0 release](https://github.com/lightvector/KataGo/releases/tag/v1.18.0), using `katago-v1.18.0-cuda12.8-cudnn9.8.0-linux-x64.zip`, not `+bs50`. The AppImage is extracted at build time; no runtime FUSE or privileged execution is required. |
| `/app/config.toml` | Repository [`config.toml.cuda`](../config.toml.cuda), copied verbatim. |
| `/app/analysis_config.cfg` | Repository [`analysis_config.cfg.cuda`](../analysis_config.cfg.cuda), copied verbatim. |
| `/app/model.bin.gz` | Pinned standard b28 model, downloaded and checksum-verified at build time. |

### Pinned Model

The Dockerfile build arguments pin both the model filename and its required
SHA-256 checksum:

| Build argument | Default |
| --- | --- |
| `KATAGO_MODEL` | `kata1-b28c512nbt-s12043015936-d5616446734.bin.gz` |
| `KATAGO_MODEL_SHA256` | `93abbeea4b4b38a6b5fda83e58927b588e2ca195ef9816b831db9d629c029efa` |

The [upstream network metadata](https://katagotraining.org/api/networks/kata1-b28c512nbt-s12043015936-d5616446734/)
provides the model download URL and checksum. This model uses format 15,
compatible with KataGo 1.18.0's supported formats 3 through 17; see upstream
[`modelversion.h`](https://github.com/lightvector/KataGo/blob/v1.18.0/cpp/neuralnet/modelversion.h).
A missing or mismatched checksum fails the build.

To select a different compatible model, override **both** `KATAGO_MODEL` and
`KATAGO_MODEL_SHA256` with `--build-arg` in a direct `docker build` or
`docker buildx build`. They are not publisher environment inputs: exporting
them has no effect on `scripts/publish-katago-cuda.sh`.

## Runtime Defaults

The dedicated server TOML sets:

| Setting | Value |
| --- | --- |
| `server.host` | `0.0.0.0` |
| `server.port` | `2718` |
| `server.max_concurrent_requests` | `8` |
| `server.request_timeout_secs` | `300` |
| `server.log_format` | `json` |
| `katago.move_timeout_secs` | `60` |
| `katago.default_max_visits` | `50` |
| `katago.max_visits_limit` | `2000` |

The dedicated engine config uses conservative single-GPU settings:

```ini
numAnalysisThreads = 2
numSearchThreadsPerAnalysisThread = 2
nnMaxBatchSize = 32
numNNServerThreadsPerModel = 1
cudaDeviceToUse = 0
nnCacheSizePowerOfTwo = 20
maxVisits = 50
```

Runtime port precedence is `KATAGO_SERVER_PORT` > `PORT` > TOML `server.port`.
When the core connects to a fixed port `2718`, keep the platform's container port
and any port overrides at `2718` as well.
The image sets `KATAGO_CONFIG_FILE=/app/config.toml` so `serve` and the built-in
healthcheck select the same server configuration. To replace the server TOML,
set `KATAGO_CONFIG_FILE` to the mounted file rather than passing `--config` only
to `serve`. `KATAGO_CONFIG_PATH` instead selects the KataGo engine `.cfg` file.

Existing `KATAGO_*` environment overrides and model/config mounts are optional
tuning mechanisms, not startup requirements; see [configuration.md](configuration.md).
Keep mounted files readable and binaries executable by UID/GID `1000:1000`;
do not mount over `/app` and hide the server. If changing the listening port,
update the container port in the local port mapping too.

The Docker `HEALTHCHECK` runs `/app/katago-server healthcheck`, probing
`/api/v1/health` with a `300s` start period for model loading and CUDA warmup.
Readiness is `/api/v1/health/ready`; liveness is `/api/v1/health/live`.

## Build Locally

From the repository root, provide the repository URL as OCI metadata. `--load`
loads the image locally and does not push it. Building and the following checks
do not require a GPU:

```bash
docker buildx build \
  --platform linux/amd64 \
  --file Dockerfile.katago-cuda \
  --build-arg SOURCE_REPOSITORY="https://github.com/<owner>/<repository>" \
  --tag katago-cuda:1.18.0 \
  --load \
  .

docker run --rm --entrypoint /usr/local/bin/katago katago-cuda:1.18.0 version
docker run --rm katago-cuda:1.18.0 --version
docker run --rm katago-cuda:1.18.0 check-config
```

These checks verify the KataGo and server executables and effective server
configuration, including bundled paths. They never start the default server
and do **not** prove model loading or CUDA inference.

## Run Locally With A GPU

Serving and actual inference require an NVIDIA GPU, a driver compatible with
the CUDA 12.8 runtime, and NVIDIA Container Toolkit. The default config uses
the first visible GPU (`cudaDeviceToUse = 0`), even when all GPUs are exposed:

```bash
docker run --rm --gpus all -p 127.0.0.1:2718:2718 katago-cuda:1.18.0
```

In another terminal, wait for readiness to return HTTP 200 before submitting
an analysis request:

```bash
curl --fail http://127.0.0.1:2718/api/v1/health/ready
curl --fail http://127.0.0.1:2718/api/v1/analysis \
  -H 'Content-Type: application/json' \
  -d '{"moves":["D4","Q16","R4"],"komi":7.5,"rules":"chinese","maxVisits":50}'
```

An actual analysis response on a GPU host is needed to verify CUDA inference;
the non-GPU checks above are not a substitute. The server has no built-in
authentication or TLS. Keep local publishing bound to loopback as above and
protect any externally reachable service with platform authentication and TLS
or a reverse proxy providing both.

## Publish To GHCR

`scripts/publish-katago-cuda.sh` is configured entirely through environment
variables. It does not parse positional arguments or flags, including `--help`
and `--dry-run`, and does not automatically load a `.env` file. Running it
builds and pushes the image, then verifies the published tags.

### Prerequisites

- Bash 4 or later.
- Docker with a running daemon and Buildx. The selected builder must support
  `linux/amd64`, and Docker must be able to run the resulting `linux/amd64` image.
- Git with an `origin` remote, unless you explicitly set `SOURCE_REPOSITORY`.
- Network access to download base images, Rust dependencies, system packages,
  the KataGo release, and the pinned model, and to push to and pull from `ghcr.io`.
- A GitHub personal access token (classic) with `write:packages`, belonging to an
  account allowed to publish packages under `GHCR_OWNER`. If an organization
  owns the package and enforces SSO, authorize the token for that organization.
  See [GitHub's authentication instructions](https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry#authenticating-to-the-container-registry).

### Quick Start

Run these commands in Bash from the repository root, replacing the example
account names. The prompt hides the token and avoids putting it in shell history:

```bash
export GHCR_OWNER="your-github-user-or-organization"
export GHCR_USERNAME="your-github-username"
read -r -s -p "GitHub token (CR_PAT): " CR_PAT
printf '\n'
export CR_PAT

bash scripts/publish-katago-cuda.sh
unset CR_PAT
```

### Required Variables

These three variables must be exported or supplied as `NAME=value` assignments
before the script command. None has a default.

| Variable | Meaning |
| --- | --- |
| `GHCR_OWNER` | GitHub user or organization namespace that will own the image, such as `my-user` or `my-org`. Do not include `ghcr.io/` or an image name. |
| `GHCR_USERNAME` | GitHub username of the account that owns the token. When publishing to an organization, this is your username, not the organization name. |
| `CR_PAT` | Personal access token (classic) with `write:packages`, not your GitHub password. This scope permits both the push and the pull used for verification. |

### Optional Variables

Unset or empty optional variables use the following defaults:

| Variable | Default | Meaning |
| --- | --- | --- |
| `IMAGE_NAME` | `katago-cuda` | Image name within the owner namespace, without a registry, path, or tag. |
| `SOURCE_REPOSITORY` | Git `origin` URL from this checkout | Source repository URL for the image's OCI metadata. Set this explicitly if Git or `origin` is unavailable, or if `origin` uses an unsupported URL format or points to the wrong repository. Accepts HTTP(S) URLs and GitHub SSH URLs; GitHub SSH URLs are converted to HTTPS and a trailing `.git` is removed. |
| `KATAGO_VERSION` | `1.18.0` | KataGo release version in `X.Y.Z` format, without a leading `v`. Used in the download URL and both image tags. |
| `KATAGO_BACKEND` | `cuda12.8-cudnn9.8.0` | Backend identifier used in the canonical image tag and the default archive filename. |
| `KATAGO_ASSET` | `katago-v${KATAGO_VERSION}-${KATAGO_BACKEND}-linux-x64.zip` | ZIP filename from the selected KataGo release, not a path or URL. The `+bs50` variant is rejected. |
| `KATAGO_SHA256` | `6d4720fed7362c8dc71e51932489a9ab53b89e9e98f2ddac339d7e0d408e6733` | Expected checksum of the downloaded ZIP, as 64 lowercase hexadecimal characters. This default is pinned to the default version and archive. |

`SOURCE_REPOSITORY` identifies the repository containing `Dockerfile.katago-cuda`,
not the image's registry destination or the upstream KataGo download location.
For example, an `origin` of `https://github.com/my-org/my-repository.git`
automatically becomes `https://github.com/my-org/my-repository`; no export is
needed in that case.

`GHCR_OWNER` and `IMAGE_NAME` are normalized to lowercase. For example, to
override the image name and source metadata while keeping the default release,
run this after exporting the required variables:

```bash
IMAGE_NAME="my-katago-cuda" \
SOURCE_REPOSITORY="https://github.com/my-org/my-repository" \
  bash scripts/publish-katago-cuda.sh
```

If you change `KATAGO_VERSION`, `KATAGO_BACKEND`, or `KATAGO_ASSET` to select a
different archive, also set `KATAGO_SHA256` to that archive's verified checksum.
The checksum is not recalculated automatically, so keeping the default will
cause the build to fail its checksum check.

Changing `KATAGO_BACKEND` does not change the CUDA base image. The Dockerfile
expects an AppImage-based archive compatible with its CUDA/cuDNN runtime. If
switching runtimes, review the `CUDA_IMAGE` default in `Dockerfile.katago-cuda`;
exporting `CUDA_IMAGE` has no effect through this script.

### Generated Variables

Do not set `VERSION_IMAGE`, `CANONICAL_IMAGE`, or the other internal variables
below. The script assigns them itself, overwriting any inherited environment
values. Only the required and optional variables listed above are user inputs.

| Internal variable | How it is calculated |
| --- | --- |
| `NORMALIZED_OWNER`, `NORMALIZED_IMAGE_NAME` | Lowercase versions of `GHCR_OWNER` and `IMAGE_NAME`. |
| `IMAGE_REPOSITORY` | `ghcr.io/${NORMALIZED_OWNER}/${NORMALIZED_IMAGE_NAME}` |
| `VERSION_TAG` | `${KATAGO_VERSION}` |
| `CANONICAL_TAG` | `${KATAGO_VERSION}-${KATAGO_BACKEND}` |
| `VERSION_IMAGE` | `${IMAGE_REPOSITORY}:${VERSION_TAG}` |
| `CANONICAL_IMAGE` | `${IMAGE_REPOSITORY}:${CANONICAL_TAG}` |
| `SCRIPT_DIR`, `REPOSITORY_ROOT`, `DOCKERFILE` | Paths resolved from the script's location. |
| `DEFAULT_KATAGO_VERSION`, `DEFAULT_KATAGO_BACKEND`, `DEFAULT_KATAGO_SHA256` | Constants defined in the script; override the corresponding `KATAGO_*` inputs instead. |
| `INSPECTED_DIGEST` | Digest read from a published image's remote manifest during verification. |

For example, with `GHCR_OWNER=my-org` and the default optional settings,
`VERSION_IMAGE` becomes `ghcr.io/my-org/katago-cuda:1.18.0` and `CANONICAL_IMAGE`
becomes `ghcr.io/my-org/katago-cuda:1.18.0-cuda12.8-cudnn9.8.0`. To change these
references, set `GHCR_OWNER`, `IMAGE_NAME`, `KATAGO_VERSION`, or `KATAGO_BACKEND`,
not the generated variables.

### Publication Behavior

The token is read from the environment and sent to `docker login` over standard
input. It is not printed, used as a build argument, or written to this
repository or an image layer. Do not enable shell tracing (`bash -x` or `set -x`)
when handling the token. Docker may retain login credentials in its configured
credential store; `unset CR_PAT` only removes the shell variable.

The first successful push creates the package;
the script does not change package visibility. A command-line-published package
may initially be private.

With the defaults, the script builds and pushes these references in one
operation, without a `latest` tag:

```text
ghcr.io/<owner>/katago-cuda:1.18.0-cuda12.8-cudnn9.8.0
ghcr.io/<owner>/katago-cuda:1.18.0
```

Names, tags, credentials, and publisher environment inputs are unchanged.
Tags identify the KataGo release/backend, not the server checkout; rebuilding
from a different checkout can change the image digest under the same tags.
Use a digest when an immutable reference to the complete image is needed.

After publication, the script inspects both remote manifests and confirms they
resolve to the same digest. It pulls the canonical tag with
`--platform linux/amd64`, verifies `linux/amd64` from `docker image inspect`'s OS
and architecture fields, and runs three explicit non-GPU checks:

```bash
docker run --rm --entrypoint /usr/local/bin/katago IMAGE version
docker run --rm IMAGE --version
docker run --rm IMAGE check-config
```

Here `IMAGE` is the canonical published reference. Verification never starts the
default server and does not prove CUDA inference. A single-image manifest need
not print a `Platform:` line in Buildx's human output. No GPU is required for
the build or these checks. Offline helper tests use mocked Docker commands:
`bash tests/publish-katago-cuda.sh` (no publication).

## Manual Verification

Use the normalized lowercase owner in these commands:

```bash
docker buildx imagetools inspect \
  "ghcr.io/<owner>/katago-cuda:1.18.0-cuda12.8-cudnn9.8.0"
docker buildx imagetools inspect \
  "ghcr.io/<owner>/katago-cuda:1.18.0"
docker pull --platform linux/amd64 \
  "ghcr.io/<owner>/katago-cuda:1.18.0-cuda12.8-cudnn9.8.0"
docker image inspect --format '{{.Os}}/{{.Architecture}}' \
  "ghcr.io/<owner>/katago-cuda:1.18.0-cuda12.8-cudnn9.8.0"
docker run --rm --entrypoint /usr/local/bin/katago \
  "ghcr.io/<owner>/katago-cuda:1.18.0-cuda12.8-cudnn9.8.0" version
docker run --rm \
  "ghcr.io/<owner>/katago-cuda:1.18.0-cuda12.8-cudnn9.8.0" --version
docker run --rm \
  "ghcr.io/<owner>/katago-cuda:1.18.0-cuda12.8-cudnn9.8.0" check-config
```

This process only builds, publishes, and verifies the complete CUDA server
image. It performs no cloud deployment and does not change deployment files or
cloud infrastructure.

# KataGo CUDA Image

`Dockerfile.katago-cuda` builds a reusable `linux/amd64` image containing the
official KataGo 1.18.0 CUDA 12.8/cuDNN 9.8.0 executable. It does not contain a
neural-network model, an analysis configuration, `katago-server`, or an HTTP
service.

This is separate from the CPU/CUDA **server** images and their versions and
configuration contracts; see [deployment.md](deployment.md#docker-images).

## Build Locally

Provide the URL of the repository containing the Dockerfile as OCI metadata:

```bash
docker buildx build \
  --platform linux/amd64 \
  --file Dockerfile.katago-cuda \
  --build-arg SOURCE_REPOSITORY="https://github.com/<owner>/<repository>" \
  --tag katago-cuda:1.18.0 \
  --load \
  .

docker run --rm katago-cuda:1.18.0
```

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
- Network access to download the base image, system packages, and KataGo release,
  and to push to and pull from `ghcr.io`.
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

After publication, the script inspects both remote manifests and confirms they
resolve to the same digest. It pulls the canonical tag with
`--platform linux/amd64`, verifies `linux/amd64` from `docker image inspect`'s OS
and architecture fields, and runs the default `katago version` command. A
single-image manifest need not print a `Platform:` line in Buildx's human output.
No GPU is required for the build or version smoke test. Offline helper tests use
mocked Docker commands: `bash tests/publish-katago-cuda.sh` (no publication).

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
docker run --rm \
  "ghcr.io/<owner>/katago-cuda:1.18.0-cuda12.8-cudnn9.8.0"
```

This process only publishes the reusable KataGo CUDA image. It performs no
cloud deployment and does not change any cloud infrastructure.

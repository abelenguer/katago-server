# Historical KataGo CUDA Image Publication Spec (Superseded)

> **Superseded:** the complete CUDA server-image requirement and
> [current image guide](docs/KATAGO_CUDA_IMAGE.md) replace this binary-only
> specification. The text below is retained only as historical context, not as
> current implementation instructions or acceptance criteria.
>
> `Dockerfile.katago-cuda` now includes `katago-server` built from the checkout,
> a checksum-pinned model, and dedicated server/analysis configs. It starts
> `/app/katago-server serve` on port 2718 by default. The old exclusions of a
> server, model, configs, and HTTP port, and the version-only default-command
> checks below no longer apply. Use the guide's explicit non-GPU checks,
> including `--entrypoint /usr/local/bin/katago IMAGE version`, instead.
>
> The official KataGo 1.18.0 CUDA release and existing GHCR publishing interface
> are retained. The main `Dockerfile` and its variants remain unchanged; no
> deployment or cloud-infrastructure changes are part of this requirement.

## 1. Purpose

Implement the repository files and commands required to:

1. Build a reusable Docker image containing the official KataGo CUDA binary.
2. Tag the image with explicit, reproducible version tags.
3. Authenticate with GitHub Container Registry (`ghcr.io`) using the user's existing GitHub personal access token.
4. Push the image to GitHub Container Registry.
5. Verify that the published image can be inspected and pulled.

This specification covers **image construction and publication only**.

---

## 2. Scope

### In scope

- A Dockerfile for KataGo with the CUDA + cuDNN backend.
- Downloading the official KataGo Linux release archive during the Docker build.
- Verifying the downloaded archive with SHA-256.
- Installing only the runtime dependencies required by the packaged KataGo executable.
- Handling the Linux AppImage packaging correctly inside Docker.
- Building for `linux/amd64`.
- Tagging the image.
- Logging in to `ghcr.io` without exposing the token.
- Pushing the image to GitHub Container Registry.
- Verifying the remote manifest and pull operation.
- Minimal documentation for the build and publication commands.

### Explicitly out of scope

Do **not** implement or modify any of the following:

- Cloud Run deployment.
- Google Cloud deployment of any kind.
- Google Artifact Registry.
- Remote Artifact Registry repositories or caches.
- Kubernetes or GKE.
- Terraform or other cloud infrastructure.
- `katago-server`.
- Any HTTP server or API.
- KataGo neural-network model downloads.
- KataGo analysis configuration.
- Local OpenCL installation or configuration.
- GitHub Actions or automated CI publication.
- Automatic GitHub package visibility changes.
- A final application image that consumes this image.

The result is only a reusable KataGo CUDA container image published to GHCR.

---

## 3. Fixed KataGo Distribution

Use the following official KataGo release unless the repository already contains an explicit newer version decision:

| Property | Value |
|---|---|
| KataGo version | `1.18.0` |
| Backend | CUDA + cuDNN |
| Platform | Linux x86-64 |
| CUDA version | `12.8` |
| cuDNN version | `9.8.0` |
| Large-board build | No |
| Release asset | `katago-v1.18.0-cuda12.8-cudnn9.8.0-linux-x64.zip` |
| SHA-256 | `6d4720fed7362c8dc71e51932489a9ab53b89e9e98f2ddac339d7e0d408e6733` |
| Download URL | `https://github.com/lightvector/KataGo/releases/download/v1.18.0/katago-v1.18.0-cuda12.8-cudnn9.8.0-linux-x64.zip` |

Do not use the `+bs50` artifact.

The version, asset name, URL, and checksum must be declared in one obvious place and must not be duplicated inconsistently across scripts.

---

## 4. Target Image

The default image coordinates must be:

```text
ghcr.io/<ghcr-owner>/katago-cuda
```

The owner must be supplied at publication time rather than hard-coded.

Publish these tags:

```text
1.18.0-cuda12.8-cudnn9.8.0
1.18.0
```

Requirements:

- The full backend tag is the canonical tag.
- The version-only tag is a convenience alias to the same image.
- Do not publish `latest`.
- Repository owner and image name must be normalized to lowercase before constructing the GHCR image reference.
- Both tags must resolve to the same image digest after publication.

---

## 5. Required Repository Artifacts

Follow the existing repository layout where one already exists. Otherwise, add:

```text
Dockerfile.katago-cuda
scripts/publish-katago-cuda.sh
```

Optionally add a short repository-local README section describing their use.

Do not add token files, credentials, generated archives, or unpacked KataGo binaries to Git.

---

## 6. Dockerfile Requirements

Create `Dockerfile.katago-cuda`.

### 6.1 Base image

Use an official NVIDIA CUDA runtime image compatible with CUDA 12.8 and cuDNN, for example:

```dockerfile
FROM nvidia/cuda:12.8.1-cudnn-runtime-ubuntu22.04
```

The implementation may pin the base image by digest if this is consistent with the repository's dependency policy.

Use a runtime image, not a CUDA development image, because KataGo is downloaded as a precompiled executable.

### 6.2 Build arguments

The Dockerfile must expose build arguments for at least:

```text
KATAGO_VERSION
KATAGO_ASSET
KATAGO_SHA256
SOURCE_REPOSITORY
```

Their defaults must match Section 3.

Changing the KataGo version in the future must require editing a small, clearly identified set of values.

### 6.3 Runtime packages

Install only the packages required to download, verify, unpack, and run KataGo. The expected set is approximately:

```text
ca-certificates
curl
unzip
libgomp1
```

Agents must add another package only when it is demonstrably required.

After installation:

- Clean the APT cache.
- Remove `/var/lib/apt/lists/*`.
- Remove the downloaded ZIP.
- Remove temporary extraction files not needed at runtime.

### 6.4 Download integrity

The Docker build must:

1. Download the archive from the official `lightvector/KataGo` GitHub release.
2. Use `curl --fail --location`.
3. Include bounded retry handling.
4. Verify the archive using the exact SHA-256 from Section 3.
5. Fail the build immediately if the checksum does not match.
6. Never continue with an unverified archive.

Expected verification pattern:

```sh
echo "${KATAGO_SHA256}  /tmp/katago.zip" | sha256sum --check -
```

### 6.5 AppImage handling

The official Linux executable is packaged using AppImage.

The image must not require FUSE at runtime and must not require privileged container execution.

The Docker build must therefore extract the AppImage during image construction, for example by invoking:

```sh
./katago --appimage-extract
```

Then:

- Move the resulting extracted application to a stable directory such as `/opt/katago/app`.
- Remove the original AppImage executable once extraction succeeds.
- Provide a stable executable named `katago` on `PATH`, normally through `/usr/local/bin/katago`.
- The wrapper must forward all arguments and preserve the exit code.

Expected wrapper behavior:

```sh
exec /opt/katago/app/AppRun "$@"
```

The implementation must inspect the actual release archive and adjust paths safely if its internal layout differs. It must not silently assume success when the expected executable or `AppRun` file is absent.

### 6.6 Image contents

The final image must contain:

- CUDA runtime libraries.
- cuDNN runtime libraries.
- The extracted KataGo application.
- A `katago` executable available through `PATH`.

The final image must not contain:

- A neural-network model.
- An analysis configuration.
- User credentials.
- The GitHub token.
- The release ZIP.
- Build caches.
- `katago-server`.
- Application-specific Dogyo files.

### 6.7 OCI metadata

Add at least these OCI labels:

```text
org.opencontainers.image.title
org.opencontainers.image.description
org.opencontainers.image.version
org.opencontainers.image.source
```

`org.opencontainers.image.source` must be supplied through `SOURCE_REPOSITORY` and should point to the repository containing this Dockerfile. This allows GitHub to associate the package with its source repository.

Do not hard-code an invented repository URL.

### 6.8 Default command

Use a harmless default command that proves KataGo is installed without starting a server, for example:

```dockerfile
CMD ["katago", "version"]
```

Do not add an HTTP service or expose a port.

---

## 7. Publication Script

Create an executable script:

```text
scripts/publish-katago-cuda.sh
```

Use Bash with:

```bash
set -euo pipefail
```

Do not enable command tracing with `set -x`, because it could expose credentials.

### 7.1 Required environment variables

Require:

```text
GHCR_OWNER
GHCR_USERNAME
CR_PAT
```

Definitions:

- `GHCR_OWNER`: GitHub user or organization that will own the package.
- `GHCR_USERNAME`: GitHub username used to authenticate.
- `CR_PAT`: Existing GitHub personal access token authorized to publish packages.

Optional variables may include:

```text
IMAGE_NAME
SOURCE_REPOSITORY
KATAGO_VERSION
```

Defaults:

```text
IMAGE_NAME=katago-cuda
KATAGO_VERSION=1.18.0
```

The script must fail early with a clear message when a required variable is absent.

### 7.2 Token requirements

The existing token is expected to be a GitHub personal access token accepted by GitHub Packages and to have at least:

```text
write:packages
```

If the owner is an organization that enforces SSO, the token must already be authorized for that organization.

The script must never:

- Print the token.
- Put the token in a command-line argument.
- Put the token in a Docker build argument.
- Put the token in an image layer.
- Write the token to the repository.
- Commit the token.
- Persist the token in a generated script or configuration file.

### 7.3 GHCR login

Authenticate using standard input:

```bash
printf '%s' "$CR_PAT" |
  docker login ghcr.io \
    --username "$GHCR_USERNAME" \
    --password-stdin
```

The script must stop if login fails.

The first successful push is expected to create the GHCR package automatically. Do not add code that attempts to create an empty package beforehand.

### 7.4 Build and push

Use Docker Buildx and build only for:

```text
linux/amd64
```

The script must build and push in a single operation:

```bash
docker buildx build \
  --platform linux/amd64 \
  --file Dockerfile.katago-cuda \
  --build-arg SOURCE_REPOSITORY="$SOURCE_REPOSITORY" \
  --tag "ghcr.io/${NORMALIZED_OWNER}/${IMAGE_NAME}:1.18.0-cuda12.8-cudnn9.8.0" \
  --tag "ghcr.io/${NORMALIZED_OWNER}/${IMAGE_NAME}:1.18.0" \
  --push \
  .
```

The final implementation must avoid duplicating version constants unnecessarily. Construct tags from named variables where practical.

Before building, validate that:

- Docker is installed.
- `docker buildx` is available.
- The selected builder supports `linux/amd64`.
- The Dockerfile exists.
- Owner and image name produce valid lowercase GHCR references.

Do not run or require a GPU for the image build.

### 7.5 Visibility

Do not attempt to change package visibility through the script.

GitHub initially creates command-line-published container packages as private unless the account's settings or a later manual action change that behavior. Visibility management is outside this implementation.

---

## 8. Verification

After the push succeeds, the script or documented verification procedure must perform the following checks.

### 8.1 Inspect the remote image

Run:

```bash
docker buildx imagetools inspect \
  "ghcr.io/<owner>/katago-cuda:1.18.0-cuda12.8-cudnn9.8.0"
```

Confirm:

- The image exists.
- A `linux/amd64` manifest is present.
- A digest is returned.

### 8.2 Verify both tags

Inspect both tags and confirm that they resolve to the same digest:

```text
1.18.0-cuda12.8-cudnn9.8.0
1.18.0
```

### 8.3 Pull the image

Run:

```bash
docker pull \
  "ghcr.io/<owner>/katago-cuda:1.18.0-cuda12.8-cudnn9.8.0"
```

This must complete successfully using the authenticated session when the package is private.

### 8.4 Verify KataGo installation

On a machine without an NVIDIA GPU, only a non-GPU smoke test is required:

```bash
docker run --rm \
  "ghcr.io/<owner>/katago-cuda:1.18.0-cuda12.8-cudnn9.8.0"
```

The default command must execute `katago version` successfully and return exit code `0`.

A real CUDA inference test is not part of this specification.

---

## 9. Error Handling

The implementation must fail clearly for at least these cases:

- Missing `GHCR_OWNER`.
- Missing `GHCR_USERNAME`.
- Missing `CR_PAT`.
- Invalid GHCR owner or image name.
- Docker daemon unavailable.
- Buildx unavailable.
- GHCR authentication failure.
- KataGo download failure.
- SHA-256 mismatch.
- ZIP extraction failure.
- AppImage extraction failure.
- Expected KataGo executable absent.
- Docker build failure.
- Push failure.
- Remote manifest not found after publication.

Error messages must identify the failed stage without printing secrets.

---

## 10. Idempotency and Reproducibility

- Re-running the publication command with unchanged inputs must produce the same image contents except for unavoidable upstream image metadata.
- Use explicit version tags; do not depend on `latest`.
- Use the fixed KataGo checksum.
- Prefer pinning the NVIDIA base-image digest if the repository normally pins external container dependencies.
- Do not download a neural network during the build.
- Do not introduce timestamps or dynamically generated files unless required.
- The script may overwrite the same GHCR tags only when intentionally invoked by the user.

---

## 11. Documentation Requirements

Document the exact manual usage.

Example:

```bash
export GHCR_OWNER="your-github-user-or-organization"
export GHCR_USERNAME="your-github-username"
export CR_PAT="your-existing-token"
export SOURCE_REPOSITORY="https://github.com/<owner>/<repository>"

./scripts/publish-katago-cuda.sh
```

The documentation must explain:

- The token is supplied only through the environment.
- The package is created by the first push.
- The image is published under `ghcr.io/<owner>/katago-cuda`.
- No model is included.
- No cloud deployment is performed.
- How to inspect and pull the image.
- How to unset the token after publication:

```bash
unset CR_PAT
```

Do not include a real username, organization, repository, or token.

---

## 12. Acceptance Criteria

The task is complete only when all of the following are true:

- [ ] `Dockerfile.katago-cuda` exists.
- [ ] The Dockerfile uses a CUDA 12.8 + cuDNN runtime base.
- [ ] It downloads the official KataGo `1.18.0` CUDA 12.8/cuDNN 9.8.0 Linux archive.
- [ ] It verifies the exact SHA-256 before extraction.
- [ ] It does not use the `+bs50` build.
- [ ] It extracts the AppImage during the Docker build.
- [ ] The runtime does not require FUSE or privileged execution.
- [ ] `katago` is available on `PATH`.
- [ ] The image contains no neural-network model or analysis configuration.
- [ ] `scripts/publish-katago-cuda.sh` exists and is executable.
- [ ] The script receives the GHCR owner, username, and token from environment variables.
- [ ] The token is passed to `docker login` through standard input.
- [ ] The token is never committed, printed, embedded, or passed as a build argument.
- [ ] The image is built for `linux/amd64`.
- [ ] The full version tag is pushed.
- [ ] The version-only alias tag is pushed.
- [ ] No `latest` tag is pushed.
- [ ] Both remote tags resolve to the same digest.
- [ ] The remote manifest reports `linux/amd64`.
- [ ] The image can be pulled from GHCR.
- [ ] Running the image's default command returns KataGo version information with exit code `0`.
- [ ] No Cloud Run, Google Artifact Registry, Kubernetes, server, model, or cloud-deployment work is included.

---

## 13. Required Agent Report

After implementation, the agents must report:

1. Files created or modified.
2. Final GHCR image reference.
3. Tags published.
4. Remote digest for each tag.
5. Whether both tags resolve to the same digest.
6. Output of the non-GPU KataGo version smoke test.
7. Any deviation from this specification and the reason.
8. Confirmation that no credentials were written to repository files or Docker layers.

Do not claim publication succeeded unless the remote manifest was inspected successfully.

---

## 14. Authoritative References

- KataGo `v1.18.0` release and assets:  
  `https://github.com/lightvector/KataGo/releases/tag/v1.18.0`
- GitHub Container Registry authentication and push documentation:  
  `https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry`
- Official NVIDIA CUDA container images:  
  `https://hub.docker.com/r/nvidia/cuda`

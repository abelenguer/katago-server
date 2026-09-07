#!/usr/bin/env bash

# Build and publish the linux/amd64 KataGo CUDA image to GHCR.
#
# Required environment variables:
#   GHCR_OWNER     GitHub user or organization that will own the image.
#   GHCR_USERNAME  GitHub username of the account that owns CR_PAT.
#   CR_PAT         GitHub personal access token (classic) with write:packages.
#
# SOURCE_REPOSITORY is optional when this checkout has a supported Git origin
# URL; otherwise, export the HTTP(S) URL of the repository with this Dockerfile.
# Image references such as VERSION_IMAGE are generated internally, not inputs.
#
# Usage from the repository root, after exporting CR_PAT:
#   GHCR_OWNER="your-user-or-org" GHCR_USERNAME="your-github-username" \
#     bash scripts/publish-katago-cuda.sh
#
# Configuration is environment-only; no arguments or flags (including --help
# or --dry-run) are handled, and .env files are not loaded. Running this script
# pushes both image tags to GHCR before verifying them.
# See docs/KATAGO_CUDA_IMAGE.md, "Publish To GHCR", for prerequisites, safe token
# entry, all optional environment variables and defaults, and checksum guidance.

set -euo pipefail

readonly DEFAULT_KATAGO_VERSION="1.18.0"
readonly DEFAULT_KATAGO_BACKEND="cuda12.8-cudnn9.8.0"
readonly DEFAULT_KATAGO_SHA256="6d4720fed7362c8dc71e51932489a9ab53b89e9e98f2ddac339d7e0d408e6733"

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPOSITORY_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
DOCKERFILE="${REPOSITORY_ROOT}/Dockerfile.katago-cuda"

die() {
    printf 'Error: %s\n' "$*" >&2
    exit 1
}

require_environment_variable() {
    local variable_name="$1"

    if [[ -z "${!variable_name:-}" ]]; then
        die "${variable_name} must be set."
    fi
}

resolve_source_repository() {
    local source="${SOURCE_REPOSITORY:-}"

    if [[ -z "${source}" ]]; then
        command -v git >/dev/null 2>&1 ||
            die "SOURCE_REPOSITORY is unset and git is unavailable to detect it."
        source="$(git -C "${REPOSITORY_ROOT}" remote get-url origin 2>/dev/null)" ||
            die "SOURCE_REPOSITORY is unset and the Git origin URL could not be detected."
    fi

    case "${source}" in
        https://*|http://*)
            ;;
        git@github.com:*)
            source="https://github.com/${source#git@github.com:}"
            ;;
        ssh://git@github.com/*)
            source="https://github.com/${source#ssh://git@github.com/}"
            ;;
        *)
            die "SOURCE_REPOSITORY must be an HTTP(S) URL or a GitHub SSH URL."
            ;;
    esac

    SOURCE_REPOSITORY="${source%.git}"
    [[ "${SOURCE_REPOSITORY}" != *[[:space:]]* ]] ||
        die "SOURCE_REPOSITORY must not contain whitespace."
}

inspect_remote_image() {
    local image_reference="$1"
    local inspect_output
    local line

    if ! inspect_output="$(docker buildx imagetools inspect "${image_reference}" 2>&1)"; then
        printf '%s\n' "${inspect_output}" >&2
        die "Remote manifest inspection failed for ${image_reference}."
    fi

    printf '%s\n' "${inspect_output}"

    if [[ ! "${inspect_output}" =~ Platform:[[:space:]]+linux/amd64([/,[:space:]]|$) ]]; then
        die "Remote manifest for ${image_reference} does not contain linux/amd64."
    fi

    INSPECTED_DIGEST=""
    while IFS= read -r line; do
        if [[ "${line}" =~ ^Digest:[[:space:]]+(sha256:[0-9a-f]{64})[[:space:]]*$ ]]; then
            INSPECTED_DIGEST="${BASH_REMATCH[1]}"
            break
        fi
    done <<< "${inspect_output}"

    [[ -n "${INSPECTED_DIGEST}" ]] ||
        die "Remote manifest for ${image_reference} did not report a digest."
}

require_environment_variable GHCR_OWNER
require_environment_variable GHCR_USERNAME
require_environment_variable CR_PAT

IMAGE_NAME="${IMAGE_NAME:-katago-cuda}"
KATAGO_VERSION="${KATAGO_VERSION:-${DEFAULT_KATAGO_VERSION}}"
KATAGO_BACKEND="${KATAGO_BACKEND:-${DEFAULT_KATAGO_BACKEND}}"
KATAGO_ASSET="${KATAGO_ASSET:-katago-v${KATAGO_VERSION}-${KATAGO_BACKEND}-linux-x64.zip}"
KATAGO_SHA256="${KATAGO_SHA256:-${DEFAULT_KATAGO_SHA256}}"
NORMALIZED_OWNER="${GHCR_OWNER,,}"
NORMALIZED_IMAGE_NAME="${IMAGE_NAME,,}"

[[ "${NORMALIZED_OWNER}" =~ ^[a-z0-9]([a-z0-9-]{0,37}[a-z0-9])?$ ]] ||
    die "GHCR_OWNER does not produce a valid lowercase GitHub namespace."
[[ "${NORMALIZED_IMAGE_NAME}" =~ ^[a-z0-9]+([._-][a-z0-9]+)*$ ]] ||
    die "IMAGE_NAME does not produce a valid lowercase container image name."
[[ "${KATAGO_VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] ||
    die "KATAGO_VERSION must use the X.Y.Z format."
[[ "${KATAGO_BACKEND}" =~ ^[a-z0-9]+([._-][a-z0-9]+)*$ ]] ||
    die "KATAGO_BACKEND is not valid in a container tag."
[[ "${KATAGO_ASSET}" =~ ^[A-Za-z0-9][A-Za-z0-9._+-]*\.zip$ ]] ||
    die "KATAGO_ASSET must be a ZIP filename without a path."
[[ "${KATAGO_ASSET}" != *+bs50* ]] ||
    die "The KataGo +bs50 artifact is not supported by this image."
[[ "${KATAGO_SHA256}" =~ ^[0-9a-f]{64}$ ]] ||
    die "KATAGO_SHA256 must be a lowercase 64-character SHA-256 value."

resolve_source_repository

[[ -f "${DOCKERFILE}" ]] || die "Dockerfile not found at ${DOCKERFILE}."
command -v docker >/dev/null 2>&1 || die "Docker is not installed or is not on PATH."
docker info >/dev/null 2>&1 || die "The Docker daemon is unavailable."
docker buildx version >/dev/null 2>&1 || die "Docker Buildx is unavailable."

if ! builder_information="$(docker buildx inspect --bootstrap 2>&1)"; then
    printf '%s\n' "${builder_information}" >&2
    die "The selected Docker Buildx builder could not be inspected."
fi
if [[ ! "${builder_information}" =~ Platforms:.*linux/amd64([,[:space:]]|$) ]]; then
    die "The selected Docker Buildx builder does not support linux/amd64."
fi

# Derived references, not environment overrides. Configure GHCR_OWNER,
# IMAGE_NAME, KATAGO_VERSION, and KATAGO_BACKEND to change these values.
IMAGE_REPOSITORY="ghcr.io/${NORMALIZED_OWNER}/${NORMALIZED_IMAGE_NAME}"
(( ${#IMAGE_REPOSITORY} <= 255 )) ||
    die "The normalized GHCR image repository exceeds 255 characters."
CANONICAL_TAG="${KATAGO_VERSION}-${KATAGO_BACKEND}"
VERSION_TAG="${KATAGO_VERSION}"
CANONICAL_IMAGE="${IMAGE_REPOSITORY}:${CANONICAL_TAG}"
VERSION_IMAGE="${IMAGE_REPOSITORY}:${VERSION_TAG}"

printf 'Logging in to ghcr.io as %s...\n' "${GHCR_USERNAME}"
if ! printf '%s' "${CR_PAT}" |
    docker login ghcr.io --username "${GHCR_USERNAME}" --password-stdin; then
    die "GHCR authentication failed."
fi

printf 'Building and pushing %s and %s...\n' "${CANONICAL_IMAGE}" "${VERSION_IMAGE}"
if ! docker buildx build \
    --platform linux/amd64 \
    --file "${DOCKERFILE}" \
    --build-arg "KATAGO_VERSION=${KATAGO_VERSION}" \
    --build-arg "KATAGO_ASSET=${KATAGO_ASSET}" \
    --build-arg "KATAGO_SHA256=${KATAGO_SHA256}" \
    --build-arg "SOURCE_REPOSITORY=${SOURCE_REPOSITORY}" \
    --tag "${CANONICAL_IMAGE}" \
    --tag "${VERSION_IMAGE}" \
    --provenance=false \
    --push \
    "${REPOSITORY_ROOT}"; then
    die "KataGo CUDA image build or push failed."
fi

printf '\nInspecting canonical tag %s...\n' "${CANONICAL_IMAGE}"
inspect_remote_image "${CANONICAL_IMAGE}"
canonical_digest="${INSPECTED_DIGEST}"

printf '\nInspecting version tag %s...\n' "${VERSION_IMAGE}"
inspect_remote_image "${VERSION_IMAGE}"
version_digest="${INSPECTED_DIGEST}"

[[ "${canonical_digest}" == "${version_digest}" ]] ||
    die "Published tags resolve to different digests (${canonical_digest} and ${version_digest})."

printf '\nPulling %s...\n' "${CANONICAL_IMAGE}"
docker pull "${CANONICAL_IMAGE}" || die "Pull verification failed for ${CANONICAL_IMAGE}."

printf '\nRunning the non-GPU KataGo version smoke test...\n'
if ! smoke_test_output="$(docker run --rm "${CANONICAL_IMAGE}" 2>&1)"; then
    printf '%s\n' "${smoke_test_output}" >&2
    die "The KataGo version smoke test failed."
fi
printf '%s\n' "${smoke_test_output}"

printf '\nPublication verified.\n'
printf 'Image: %s\n' "${IMAGE_REPOSITORY}"
printf 'Tag %s: %s\n' "${CANONICAL_TAG}" "${canonical_digest}"
printf 'Tag %s: %s\n' "${VERSION_TAG}" "${version_digest}"

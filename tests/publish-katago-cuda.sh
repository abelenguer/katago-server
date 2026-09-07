#!/usr/bin/env bash
# Offline tests: source helpers only; Docker has no external-command fallback.
set -euo pipefail

TEST_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
TEST_IMAGE="ghcr.io/example/katago-cuda:1.18.0"
TEST_DIGEST="sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
MOCK_MANIFEST=""
MOCK_PLATFORM="linux/amd64"
MOCK_REMOTE_STATUS=0
MOCK_PULL_STATUS=0
MOCK_INSPECT_STATUS=0
MOCK_PULLED=false

docker() {
    case "$*" in
        "buildx imagetools inspect ${TEST_IMAGE}")
            printf '%s\n' "${MOCK_MANIFEST}"
            return "${MOCK_REMOTE_STATUS}"
            ;;
        "pull --platform linux/amd64 ${TEST_IMAGE}")
            MOCK_PULLED=true
            return "${MOCK_PULL_STATUS}"
            ;;
        "image inspect --format {{.Os}}/{{.Architecture}} ${TEST_IMAGE}")
            [[ "${MOCK_PULLED}" == true ]] || return 98
            printf '%s\n' "${MOCK_PLATFORM}"
            return "${MOCK_INSPECT_STATUS}"
            ;;
        *)
            printf 'Unexpected Docker command: %s\n' "$*" >&2
            return 99
            ;;
    esac
}

# shellcheck source=scripts/publish-katago-cuda.sh
source "${TEST_ROOT}/scripts/publish-katago-cuda.sh"

expect_failure() {
    local expected="$1" output
    shift
    if output="$("$@" 2>&1)"; then
        printf 'Expected failure: %s\n' "$*" >&2
        exit 1
    fi
    if [[ "${output}" != *"${expected}"* ]]; then
        printf 'Expected %s, got: %s\n' "${expected}" "${output}" >&2
        exit 1
    fi
}

# A single manifest (including --provenance=false builds) has no Platform line.
MOCK_MANIFEST="Name: ${TEST_IMAGE}
MediaType: application/vnd.docker.distribution.manifest.v2+json
Digest: ${TEST_DIGEST}"
inspect_remote_image "${TEST_IMAGE}" >/dev/null
[[ "${INSPECTED_DIGEST}" == "${TEST_DIGEST}" ]]
pull_and_verify_image "${TEST_IMAGE}"
[[ "${MOCK_PULLED}" == true ]]

# An index can also include non-runnable attestation manifests.
MOCK_MANIFEST="Name: ${TEST_IMAGE}
MediaType: application/vnd.oci.image.index.v1+json
Digest: ${TEST_DIGEST}
Manifests:
  Platform: linux/amd64
  Platform: unknown/unknown"
MOCK_PULLED=false
inspect_remote_image "${TEST_IMAGE}" >/dev/null
[[ "${INSPECTED_DIGEST}" == "${TEST_DIGEST}" ]]
pull_and_verify_image "${TEST_IMAGE}"
[[ "${MOCK_PULLED}" == true ]]

# Human output cannot override the actual pulled platform.
for MOCK_PLATFORM in linux/arm64 windows/amd64 linux/amd64/v2 ""; do
    expect_failure "expected linux/amd64" pull_and_verify_image "${TEST_IMAGE}"
done
MOCK_PLATFORM=linux/amd64

MOCK_PULL_STATUS=1
expect_failure "Pull verification failed" pull_and_verify_image "${TEST_IMAGE}"
MOCK_PULL_STATUS=0
MOCK_INSPECT_STATUS=1
expect_failure "Pulled image inspection failed" pull_and_verify_image "${TEST_IMAGE}"
MOCK_INSPECT_STATUS=0

MOCK_REMOTE_STATUS=1
expect_failure "Remote manifest inspection failed" inspect_remote_image "${TEST_IMAGE}"
MOCK_REMOTE_STATUS=0
for MOCK_MANIFEST in "Name: ${TEST_IMAGE}" "Digest: sha256:invalid"; do
    expect_failure "did not report a digest" inspect_remote_image "${TEST_IMAGE}"
done

printf 'KataGo CUDA verification helper tests passed (Docker mocked; no publication).\n'

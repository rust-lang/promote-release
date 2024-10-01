#!/bin/bash

# This script is executed at the start of each local release for Rustup, and
# prepares the environment by copying the artifacts built by CI onto the MinIO
# instance. Then, it starts promote-release with the right flags.

set -euo pipefail
IFS=$'\n\t'

RUSTUP_REPO="https://github.com/rust-lang/rustup"
RUSTUP_DEFAULT_BRANCH="master"

# S3 bucket from which to download the Rustup artifacts
S3_BUCKET="rustup-builds"

# CDN from which to download the CI artifacts
DOWNLOAD_BASE="https://rustup-builds.rust-lang.org"

# The artifacts for the following targets will be downloaded and copied during
# the release process. At least one target is required.
DOWNLOAD_TARGETS=(
    "x86_64-unknown-linux-gnu"
)

# The following files will be downloaded and put into the local MinIO instance.
DOWNLOAD_FILES=(
    "rustup-init"
    "rustup-init.sha256"
    "rustup-setup"
    "rustup-setup.sha256"
)

channel="$1"
override_commit="$2"

if [[ "${override_commit}" = "" ]]; then
    echo "==> detecting the last Rustup commit on the default branch"
    commit="$(git ls-remote "${RUSTUP_REPO}" | grep "refs/heads/${RUSTUP_DEFAULT_BRANCH}" | awk '{print($1)}')"
else
    echo "=>> using overridden commit ${override_commit}"
    commit="${override_commit}"
fi

for target in "${DOWNLOAD_TARGETS[@]}"; do
    if ! mc stat "local/artifacts/builds/${commit}/dist/${target}" >/dev/null 2>&1; then
        echo "==> copying ${target} from S3"

        for file in "${DOWNLOAD_FILES[@]}"; do
            if curl -Lo /tmp/component "${DOWNLOAD_BASE}/${commit}/dist/${target}/${file}" --fail; then
                mc cp /tmp/component "local/artifacts/builds/${commit}/dist/${target}/${file}" >/dev/null
            fi
        done
    else
        echo "==> reusing cached ${target} target"
    fi
done

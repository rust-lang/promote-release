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

# Build the promote-release binary if it hasn't been pre-built
if [[ ! -f "/src/target/release/promote-release" ]]; then
    echo "==> building promote-release"
    cd /src
    cargo build --release
    cd ..
fi

echo "==> configuring the environment"

# Release Rustup
export PROMOTE_RELEASE_ACTION="promote-rustup"

# Point to the right GnuPG environment
export GNUPGHOME=/persistent/gpg-home

## Environment variables also used in prod releases
export AWS_ACCESS_KEY_ID="access_key"
export AWS_SECRET_ACCESS_KEY="secret_key"
export PROMOTE_RELEASE_CHANNEL="${channel}"
export PROMOTE_RELEASE_CLOUDFRONT_DOC_ID=""
export PROMOTE_RELEASE_CLOUDFRONT_STATIC_ID=""
export PROMOTE_RELEASE_DOWNLOAD_BUCKET="rustup-builds"
export PROMOTE_RELEASE_DOWNLOAD_DIR="builds"
export PROMOTE_RELEASE_GPG_KEY_FILE=""
export PROMOTE_RELEASE_GPG_PASSWORD_FILE=""
export PROMOTE_RELEASE_UPLOAD_ADDR=""
export PROMOTE_RELEASE_UPLOAD_BUCKET="static"
export PROMOTE_RELEASE_UPLOAD_STORAGE_CLASS="STANDARD"
export PROMOTE_RELEASE_UPLOAD_DIR="rustup"

## Environment variables used only by local releases
export PROMOTE_RELEASE_S3_ENDPOINT_URL="http://minio:9000"

# Conditional environment variables
if [[ "${override_commit}" != "" ]]; then
   export PROMOTE_RELEASE_OVERRIDE_COMMIT="${override_commit}"
fi

# Conditionally set a version for the next Rustup release
if [[ "${RUSTUP_OVERRIDE_VERSION:-}" != "" ]]; then
  export PROMOTE_RELEASE_RUSTUP_OVERRIDE_VERSION="${RUSTUP_OVERRIDE_VERSION}"
fi

echo "==> starting promote-release"
/src/target/release/promote-release /persistent/release "${channel}"

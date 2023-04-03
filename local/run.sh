#!/bin/bash
# This script is executed at the start of each local release, and prepares the
# environment by copying the artifacts built by CI on the MinIO instance. Then,
# it starts promote-release with the right flags.

set -euo pipefail
IFS=$'\n\t'

RUSTC_REPO="https://github.com/rust-lang/rust"
RUSTC_DEFAULT_BRANCH="master"

# CDN to download CI artifacts from.
DOWNLOAD_BASE="https://ci-artifacts.rust-lang.org/rustc-builds"
# Rustup components to download for each target we want to release.
DOWNLOAD_COMPONENTS=(
    "build-manifest"
    "cargo"
    "rust"
    "rust-docs"
    "rust-std"
    "rustc"
)
# Targets to download the components of. *All* the components will be
# downloaded for each target, so adding more of them might slow down running
# the release process.
#
# Never remove "x86_64-unknown-linux-gnu", as that target is required for the
# release process to work (promote-release extracts version numbers from it).
DOWNLOAD_COMPONENT_TARGETS=(
    "x86_64-unknown-linux-gnu"
)
# These components are necessary for build-manifest to run, as they're fallback
# targets from a host of tier-2 triples. See build-manifest/src/main.rs
# DOCS_FALLBACK.
DOWNLOAD_DOCS_TARGETS=(
    "x86_64-apple-darwin"
    "aarch64-unknown-linux-gnu"
)
# Files to download that are not rustup components. No mangling is done on the
# file name, so include its full path.
DOWNLOAD_STANDALONE=(
    "toolstates-linux.json"
)

channel="$1"
override_commit="$2"

# Nightly is on the default branch
if [[ "${channel}" = "nightly" ]]; then
    branch="${RUSTC_DEFAULT_BRANCH}"
else
    branch="${channel}"
fi

echo "==> overriding files to force promote-release to run"
mc cp "/src/local/channel-rust-${channel}.toml" "local/static/dist/channel-rust-${channel}.toml" >/dev/null

if [[ "${override_commit}" = "" ]]; then
    echo "==> detecting the last rustc commit on branch ${branch}"
    commit="$(git ls-remote "${RUSTC_REPO}" | grep "refs/heads/${branch}" | awk '{print($1)}')"
else
    echo "=>> using overridden commit ${override_commit}"
    commit="${override_commit}"
fi

# While the nightly and beta channels have the channel name as the "release" in
# the archive names, the stable channel uses the actual Rust and Cargo version
# numbers. This hacky piece of code detects them.
if [[ "${channel}" = "stable" ]]; then
    raw_url="https://raw.githubusercontent.com/rust-lang/rust/${commit}"

    echo "==> loading rust version from src/version"
    release="$(curl --fail "${raw_url}/src/version" 2>/dev/null || true)"

    echo "found rust version ${release}"
else
    release="${channel}"
fi

download() {
    file="$1"
    if ! mc stat "local/artifacts/builds/${commit}/${file}" >/dev/null 2>&1; then
        echo "==> copying ${file} from ci-artifacts.rust-lang.org"
        if curl -Lo /tmp/component "${DOWNLOAD_BASE}/${commit}/${file}" --fail; then
            mc cp /tmp/component "local/artifacts/builds/${commit}/${file}" >/dev/null
        fi
    else
        echo "==> reusing cached ${file}"
    fi
}

for target in "${DOWNLOAD_COMPONENT_TARGETS[@]}"; do
    for component in "${DOWNLOAD_COMPONENTS[@]}"; do
        download "${component}-${release}-${target}.tar.xz"
    done
done
for target in "${DOWNLOAD_DOCS_TARGETS[@]}"; do
    download "rust-docs-${release}-${target}.tar.xz"
done
for file in "${DOWNLOAD_STANDALONE[@]}"; do
    download "${file}"
done

echo "==> configuring the environment"
# Point to the right GnuPG environment
export GNUPGHOME=/persistent/gpg-home
# Environment variables also used in prod releases
export AWS_ACCESS_KEY_ID="access_key"
export AWS_SECRET_ACCESS_KEY="secret_key"
export PROMOTE_RELEASE_CHANNEL="${channel}"
export PROMOTE_RELEASE_CLOUDFRONT_DOC_ID="id_doc_rust_lang_org"
export PROMOTE_RELEASE_CLOUDFRONT_STATIC_ID="id_static_rust_lang_org"
export PROMOTE_RELEASE_DOWNLOAD_BUCKET="artifacts"
export PROMOTE_RELEASE_DOWNLOAD_DIR="builds"
export PROMOTE_RELEASE_GPG_KEY_FILE="/persistent/gpg-key"
export PROMOTE_RELEASE_GPG_PASSWORD_FILE="/persistent/gpg-password"
export PROMOTE_RELEASE_UPLOAD_ADDR="http://localhost:9000/static"
export PROMOTE_RELEASE_UPLOAD_BUCKET="static"
export PROMOTE_RELEASE_UPLOAD_STORAGE_CLASS="STANDARD"
export PROMOTE_RELEASE_UPLOAD_DIR="dist"
# Enable xz recompression to check it in CI
export PROMOTE_RELEASE_RECOMPRESS_XZ=1
# Environment variables used only by local releases
export PROMOTE_RELEASE_BYPASS_STARTUP_CHECKS="1"
export PROMOTE_RELEASE_GZIP_COMPRESSION_LEVEL="1" # Faster recompressions
export PROMOTE_RELEASE_S3_ENDPOINT_URL="http://minio:9000"
export PROMOTE_RELEASE_SKIP_CLOUDFRONT_INVALIDATIONS="yes"
# Conditional environment variables
if [[ "${override_commit}" != "" ]]; then
    export PROMOTE_RELEASE_OVERRIDE_COMMIT="${override_commit}"
fi

echo "==> starting promote-release"
/src/target/release/promote-release /persistent/release "${channel}"

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
# Files to download that are not rustup components. No mangling is done on the
# file name, so include its full path.
DOWNLOAD_STANDALONE=(
    "toolstates-linux.json"
)

channel="$1"

# Nightly is on the default branch
if [[ "${channel}" = "nightly" ]]; then
    branch="${RUSTC_DEFAULT_BRANCH}"
else
    branch="${channel}"
fi

echo "==> overriding files to force promote-release to run"
mc cp "/src/local/channel-rust-${channel}.toml" "local/static/dist/channel-rust-${channel}.toml" >/dev/null

echo "==> detecting the last rustc commit on branch ${branch}"
commit="$(git ls-remote "${RUSTC_REPO}" | grep "refs/heads/${branch}" | awk '{print($1)}')"

download() {
    file="$1"
    if ! mc stat "local/artifacts/builds/${commit}/${file}" >/dev/null 2>&1; then
        echo "==> copying ${file} from ci-artifacts.rust-lang.org"
        curl -Lo /tmp/component "${DOWNLOAD_BASE}/${commit}/${file}" --fail
        mc cp /tmp/component "local/artifacts/builds/${commit}/${file}" >/dev/null
    else
        echo "==> reusing cached ${file}"
    fi
}

for target in "${DOWNLOAD_COMPONENT_TARGETS[@]}"; do
    for component in "${DOWNLOAD_COMPONENTS[@]}"; do
        download "${component}-${channel}-${target}.tar.xz"
    done
done
for file in "${DOWNLOAD_STANDALONE[@]}"; do
    download "${file}"
done

echo "==> starting promote-release"
export GNUPGHOME=/persistent/gpg-home
export PROMOTE_RELEASE_SKIP_CLOUDFRONT_INVALIDATIONS=yes
export PROMOTE_RELEASE_SKIP_DELETE_BUILD_DIR=yes
/src/target/release/promote-release /persistent/release "${channel}" /src/local/secrets.toml

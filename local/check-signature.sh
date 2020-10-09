#!/bin/bash

set -euo pipefail
IFS=$'\n\t'

if [[ $# -ne 1 ]]; then
    echo "usage: $0 <channel>"
    exit 1
fi
channel="$1"

# Work in a temporary directory
dir="$(mktemp -d)"
trap "rm -rf ${dir}" EXIT
cd "${dir}"

# Use MinIO's CLI to download the files from storage
mc cp "local/static/dist/channel-rust-${channel}.toml" . >/dev/null
mc cp "local/static/dist/channel-rust-${channel}.toml.asc" . >/dev/null

export GNUPGHOME="/persistent/gpg-home"
gpg --armor --pinentry-mode loopback --verify "channel-rust-${channel}.toml.asc" "channel-rust-${channel}.toml"

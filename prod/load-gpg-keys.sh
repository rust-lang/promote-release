#!/bin/bash
# Load the GPG key used by promote-release to sign release artifacts from AWS.
# Elevated AWS privileges are needed to run this script.

set -euo pipefail
IFS=$'\n\t'

mkdir -p /tmp/gnupg

aws s3 cp --recursive s3://rust-release-keys/ /tmp/gnupg/
for key in /tmp/gnupg/keys/*.asc; do
    gpg --armor --batch --pinentry-mode loopback --import "${key}"
done

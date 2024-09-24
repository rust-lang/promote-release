#!/bin/bash

# This script is executed at the start of each local release for Rustup, and
# prepares the environment by copying the artifacts built by CI onto the MinIO
# instance. Then, it starts promote-release with the right flags.

set -euo pipefail
IFS=$'\n\t'

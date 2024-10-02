#!/bin/bash
# Start a dummy local release process, without making changes to any production
# system. This requires docker and docker-compose to be installed.

set -euo pipefail
IFS=$'\n\t'

if [[ "$#" -lt 1 ]]; then
  echo "Usage: $0 <release|rustup>"
  exit 1
fi
command="$1"

if [[ "${command}" == "release" ]]; then
  if [[ "$#" -lt 2 ]] || [[ "$#" -gt 3 ]]; then
    echo "Usage: $0 release <stable|dev|nightly> [commit]"
    exit 1
  fi
fi

if [[ "${command}" == "rustup" ]]; then
  if [[ "$#" -ne 2 ]]; then
    echo "Usage: $0 rustup <stable|dev> [commit]"
    exit 1
  fi
fi

channel="$2"
override_commit="${3-}"

container_id="$(docker compose ps -q local)"
if [[ "${container_id}" == "" ]]; then
    container_status="missing"
else
    container_status="$(docker inspect "${container_id}" --format "{{.State.Status}}")"
fi
if [[ "${container_status}" != "running" ]]; then
    echo "Error: the local environment is not running!"
    echo "You can start it by running in a new terminal the following command:"
    echo
    echo "    docker compose up"
    echo
    exit 1
fi

# Pre-built the binary if the host and Docker environments match
if [[ "$(uname)" == "Linux" ]]; then
    cargo build --release
fi

# Run the command inside the docker environment.
docker compose exec -T local "/src/local/${command}.sh" "${channel}" "${override_commit}"

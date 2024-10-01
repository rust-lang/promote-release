#!/bin/bash
# Start a dummy local release process, without making changes to any production
# system. This requires docker and docker-compose to be installed.

set -euo pipefail
IFS=$'\n\t'

if [[ "$#" -lt 1 ]] || [[ "$#" -gt 2 ]]; then
    echo "Usage: $0 <channel> [commit]"
    exit 1
fi
channel="$1"
override_commit="${2-}"

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

# Ensure the release build is done
cargo build --release

# Run the command inside the docker environment.
docker compose exec -T local /src/local/run.sh "${channel}" "${override_commit}"

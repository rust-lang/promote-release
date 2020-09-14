#!/bin/bash
# Start a dummy local release process, without making changes to any production
# system. This requires docker and docker-compose to be installed.

set -euo pipefail
IFS=$'\n\t'

if [[ "$#" -ne 1 ]]; then
    echo "Usage: $0 <channel>"
    exit 1
fi
channel="$1"

container_id="$(docker-compose ps -q local)"
container_status="$(docker inspect "${container_id}" --format "{{.State.Status}}")"
if [[ "${container_status}" != "running" ]]; then
    echo "Error: the local environment is not running!"
    echo "You can start it by running in a new terminal the following command:"
    echo
    echo "    docker-compose up"
    echo
    exit 1
fi

# Ensure the release build is done
cargo build --release

# Run the command inside the docker environment.
docker-compose exec local /src/local/run.sh "${channel}"

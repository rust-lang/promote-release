#!/bin/bash
# Setup the local container when it boots, configuring access to MinIO and
# generating a GPG key when needed.

set -euo pipefail
IFS=$'\n\t'

MINIO_HOST="minio"
MINIO_PORT="9000"
MINIO_URL="http://${MINIO_HOST}:${MINIO_PORT}"
MINIO_ACCESS_KEY="access_key"
MINIO_SECRET_KEY="secret_key"

MINIO_BUCKETS=( "static" "artifacts" )

# Quit immediately when docker-compose receives a Ctrl+C
trap exit EXIT

# Wait until minio finished loading
echo "waiting for minio to start"
while ! curl --silent --fail "${MINIO_URL}/minio/health/live"; do
    sleep 0.1
done
echo "minio is now available"

echo "starting a proxy for minio"
socat "tcp-listen:${MINIO_PORT},reuseaddr,fork" "tcp:${MINIO_HOST}:${MINIO_PORT}" &

# Configure the minio client to talk to the right instance.
echo "configuring cli access to minio"
mc alias set local "${MINIO_URL}" "${MINIO_ACCESS_KEY}" "${MINIO_SECRET_KEY}" >/dev/null

# Create and configure minio buckets
for bucket in "${MINIO_BUCKETS[@]}"; do
    if ! mc stat "local/${bucket}" >/dev/null 2>&1; then
        echo "creating the ${bucket} minio bucket"
        mc mb "local/${bucket}" >/dev/null
    fi
    echo "making the ${bucket} minio bucket public"
    mc policy set download "local/${bucket}" >/dev/null
done

# Generate the GPG key to sign binaries
export GNUPGHOME=/persistent/gpg-home
if ! [[ -d "${GNUPGHOME}" ]]; then
    mkdir /persistent/gpg-home
    chmod 0700 /persistent/gpg-home
fi
if ! gpg --list-secret-keys 2>/dev/null | grep "promote-release@example.com" >/dev/null 2>&1; then
    echo "generating a dummy gpg key for signing"
    echo "password" > /persistent/gpg-password
    # https://www.gnupg.org/documentation//manuals/gnupg/Unattended-GPG-key-generation.html
    gpg --batch --gen-key /src/local/generate-gpg-key.conf >/dev/null
else
    echo "reusing existing gpg key"
fi

# Ensure there is a copy of the key on disk
if ! [[ -f /persistent/gpg-key ]]; then
    echo "dumping the gpg key to disk"
    cat /persistent/gpg-password | gpg \
        --pinentry-mode loopback \
        --passphrase-fd 0 \
        --batch \
        --armor \
        --export-secret-key promote-release@example.com \
        > /persistent/gpg-key
else
    echo "gpg key already dumped to disk"
fi

cat <<EOF

####################################################
##  Local environment bootstrapped successfully!  ##
####################################################

To start the release process locally, run either:

    ./run.sh nightly
    ./run.sh beta
    ./run.sh stable

To use a release produced locally, set this environment variable when
interacting with rustup:

    RUSTUP_DIST_SERVER="http://localhost:9000/static"

Press Ctrl-C to stop the local environment.

EOF

$@

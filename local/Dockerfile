# Container image to run the release process on. This is meant to be used
# locally, and should not be relied upon for production.

# Downloading the `mc` client from dl.min.io is unbearably slow from Europe (it
# regularly takes 2 minutes to download 20MB of binary). The only other way
# they distribute the CLI is from Docker, so we load their image as a stage and
# then copy the binary from it later in the build.
FROM minio/mc AS mc

FROM ubuntu:focal

RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y \
    awscli \
    build-essential \
    curl \
    git \
    gnupg \
    jq \
    python3 \
    nginx

# Install rustup while removing the pre-installed stable toolchain.
RUN curl https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init >/tmp/rustup-init && \
    chmod +x /tmp/rustup-init && \
    /tmp/rustup-init -y --no-modify-path --profile minimal --default-toolchain stable && \
    /root/.cargo/bin/rustup toolchain remove stable
ENV PATH=/root/.cargo/bin:$PATH

COPY --from=mc /usr/bin/mc /usr/local/bin/mc
RUN chmod 0755 /usr/local/bin/mc

ENTRYPOINT ["/src/local/setup.sh"]
CMD ["/src/local/idle.sh"]

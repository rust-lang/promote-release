# Container image to run the release process on. This is meant to be used
# locally, and should not be relied upon for production.

# Downloading the `mc` client from dl.min.io is unbearably slow from Europe (it
# regularly takes 2 minutes to download 20MB of binary). The only other way
# they distribute the CLI is from Docker, so we load their image as a stage and
# then copy the binary from it later in the build.
FROM quay.io/minio/mc:RELEASE.2025-04-16T18-13-26Z AS mc

FROM ubuntu:24.04

RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y \
    build-essential \
    curl \
    git \
    gnupg \
    jq \
    python3 \
    unzip \
    socat

# Install awscli
RUN curl --fail "https://awscli.amazonaws.com/awscli-exe-linux-$(uname -m).zip" -o "awscliv2.zip" && \
    unzip awscliv2.zip && \
    ./aws/install

# Install rustup while removing the pre-installed stable toolchain.
RUN curl https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init >/tmp/rustup-init && \
    chmod +x /tmp/rustup-init && \
    /tmp/rustup-init -y --no-modify-path --profile minimal --default-toolchain stable && \
    /root/.cargo/bin/rustup toolchain remove stable
ENV PATH=/root/.cargo/bin:$PATH

COPY --from=mc /usr/bin/mc /usr/local/bin/mc
RUN chmod 0755 /usr/local/bin/mc

# The default is 20 but slightly bumping it here. Mostly to test that this
# command works. In production we set a much higher value (150).
RUN aws configure set default.s3.max_concurrent_requests 25

ENTRYPOINT ["/src/local/setup.sh"]
CMD ["/src/local/idle.sh"]

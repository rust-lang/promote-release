#############################
#  Source code build image  #
#############################

FROM ubuntu:24.04 AS build

RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y \
    ca-certificates \
    curl \
    build-essential \
    pkg-config \
    libssl-dev

    # Install the currently pinned toolchain with rustup
RUN curl https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init >/tmp/rustup-init && \
    chmod +x /tmp/rustup-init && \
    /tmp/rustup-init -y --no-modify-path --default-toolchain stable
ENV PATH=/root/.cargo/bin:$PATH

# Build the dependencies in a separate step to avoid rebuilding all of them
# every time the source code changes. This takes advantage of Docker's layer
# caching, and it works by copying the Cargo.{toml,lock} with dummy source code
# and doing a full build with it.
WORKDIR /tmp/source
COPY Cargo.lock Cargo.toml build.rs /tmp/source/
RUN mkdir -p /tmp/source/src && \
    echo "fn main() {}" > /tmp/source/src/main.rs
RUN cargo fetch
RUN cargo build --release

# Dependencies are now cached, copy the actual source code and do another full
# build. The touch on all the .rs files is needed, otherwise cargo assumes the
# source code didn't change thanks to mtime weirdness.
RUN rm -rf /tmp/source/src
COPY src /tmp/source/src
RUN find -name "*.rs" -exec touch {} \; && cargo build --release

##################
#  Output image  #
##################

FROM ubuntu:24.04 AS binary

RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y \
    build-essential \
    ca-certificates \
    curl \
    git \
    gnupg \
    unzip

# Install rustup while removing the pre-installed stable toolchain.
RUN curl https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init >/tmp/rustup-init && \
    chmod +x /tmp/rustup-init && \
    /tmp/rustup-init -y --no-modify-path --profile minimal --default-toolchain stable && \
    /root/.cargo/bin/rustup toolchain remove stable
ENV PATH=/root/.cargo/bin:$PATH

# Install a recent AWS CLI v2, as the one included in the Ubuntu repositories is old.
RUN mkdir /tmp/awscli && \
    cd /tmp/awscli && \
    curl "https://awscli.amazonaws.com/awscli-exe-linux-x86_64.zip" -o awscli.zip && \
    unzip -q awscli.zip && \
    ./aws/install && \
    cd / && \
    rm -rf /tmp/awscli

RUN aws configure set default.s3.max_concurrent_requests 150
RUN aws configure set default.s3.max_queue_size 10000

COPY --from=build /tmp/source/target/release/promote-release /usr/local/bin/
COPY prod/load-gpg-keys.sh /usr/local/bin/load-gpg-keys

CMD ["promote-release"]

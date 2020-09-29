#############################
#  Source code build image  #
#############################

FROM ubuntu:focal AS build

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
COPY Cargo.lock Cargo.toml /tmp/source/
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

FROM ubuntu:focal AS binary

RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y \
    awscli \
    ca-certificates \
    curl \
    git \
    gnupg

# Dependencies needed to run ./x.py hash-and-dist
# These should not be needed anymore once we stop calling it through ./x.py
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y \
    build-essential \
    cmake \
    ninja-build \
    python-is-python3

COPY --from=build /tmp/source/target/release/promote-release /usr/local/bin/
COPY prod/load-gpg-keys.sh /usr/local/bin/load-gpg-keys

CMD ["promote-release"]

# promote-release

`promote-release` is the tool used by the Rust project to publish new releases
of the Rust toolchain.

## Running the tool locally

It's possible to run the `promote-release` tool locally without access to any
production credential, to ease testing changes made to it. You need to make
sure to have `docker` and `docker-compose` installed on your local system, and
you need to start the local environment by running:

```
docker-compose up
```

This will start an instance of [MinIO](https://min.io) and build a local
container tailored to run the release process on. Once the local environment is
up and running, you can start a release with one of the following commands:

```
./run.sh nightly
./run.sh beta
./run.sh stable
```

Once the release is done, you can use it with `rustup` by setting the following
environment variable while calling `rustup`:

```
RUSTUP_DIST_SERVER="http://localhost:9000/static"
```

### Adding additional files to the local release

To save on time and bandwidth, when running a release locally the tooling won't
include all files present in a proper release, but to save on bandwidth and
storage only a small subset of it is included (on 2020-09-16 a full release
weights 27GB).

You can add additional files by tweaking the environment variables in
`local/run.sh`.

## License

The contents of this repository are licensed under both the MIT and the Apache
2.0 license, allowing you to choose which one to adhere to.

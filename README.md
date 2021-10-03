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

There are multiple modes you can test locally through the `./run.sh` script, by
passing the mode name as the second argument. Each mode configures the local
environment differently, to simulate different production scenarios. The modes
currently available are:

* `standard`: most of the Rust releases, including regular nightlies, betas or
  stables.
* `security`: private releases done to address security issues, fetching from
  private artifacts buckets and private git repositories.

You can also release a specific commit by providing its full hash as the third
argument of `./run.sh`:

```
./run.sh nightly standard 0000000000000000000000000000000000000000
```

### Adding additional files to the local release

To save on time and bandwidth, when running a release locally the tooling won't
include all files present in a proper release, but to save on bandwidth and
storage only a small subset of it is included (on 2020-09-16 a full release
weights 27GB).

You can add additional files by tweaking the environment variables in
`local/run.sh`.

### Inspecting the contents of the object storage

You can access the contents of the object storage by visiting
<http://localhost:9000/minio> and logging in with:

* Access Key: `access_key`
* Secret Key: `secret_key`

## License

The contents of this repository are licensed under both the MIT and the Apache
2.0 license, allowing you to choose which one to adhere to.

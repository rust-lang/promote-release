use crate::Context;
use anyhow::{Context as _, Error};
use git2::Cred;
use std::env::VarError;
use std::str::FromStr;

const ENVIRONMENT_VARIABLE_PREFIX: &str = "PROMOTE_RELEASE_";

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum Channel {
    Stable,
    Beta,
    Nightly,
}

impl Channel {
    pub(crate) fn release_name(&self, ctx: &Context) -> String {
        if *self == Channel::Stable {
            ctx.current_version.clone().unwrap()
        } else {
            self.to_string()
        }
    }
}

impl FromStr for Channel {
    type Err = Error;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "stable" => Ok(Channel::Stable),
            "beta" => Ok(Channel::Beta),
            "nightly" => Ok(Channel::Nightly),
            _ => anyhow::bail!("unknown channel: {}", input),
        }
    }
}

impl std::fmt::Display for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Channel::Stable => "stable",
            Channel::Beta => "beta",
            Channel::Nightly => "nightly",
        })
    }
}

pub(crate) struct Config {
    /// The channel we're currently releasing.
    pub(crate) channel: Channel,
    /// CloudFront distribution ID for doc.rust-lang.org.
    pub(crate) cloudfront_doc_id: String,
    /// CloudFront distribution ID for static.rust-lang.org.
    pub(crate) cloudfront_static_id: String,
    /// The S3 bucket that CI artifacts will be downloaded from.
    pub(crate) download_bucket: String,
    /// The S3 directory that CI artifacts will be downloaded from.
    pub(crate) download_dir: String,
    /// Path to the file containing the ASCII-armored, encrypted GPG secret key.
    pub(crate) gpg_key_file: String,
    /// Path of the file containing the password of the GPG secret key.
    pub(crate) gpg_password_file: String,
    // Number of concurrent threads to start during the parallel segments of promote-release.
    pub(crate) num_threads: usize,
    /// URL of the git repository containing the Rust source code.
    pub(crate) repository: String,
    /// Username and password to authenticate with the git repository, separated by a colon.
    /// Example: `pietro:Super5ecretP@ssword`
    pub(crate) repository_authentication: Option<RepositoryCredentials>,
    /// Remote HTTP host artifacts will be uploaded to. Note that this is *not* the same as what's
    /// configured in `config.toml` for rustbuild, it's just the *host* that we're uploading to and
    /// going to be looking at urls from.
    ///
    /// This is used in a number of places such as:
    ///
    /// * Downloading manifestss * Urls in manifests
    ///
    /// and possibly more. Note that most urls end up appending PROMOTE_RELEASE_UPLOAD_DIR to this
    /// address specified. This address should not have a trailing slash.
    pub(crate) upload_addr: String,
    /// The S3 bucket that release artifacts will be uploaded to.
    pub(crate) upload_bucket: String,
    /// The S3 directory that release artifacts will be uploaded to.
    pub(crate) upload_dir: String,
    /// Whether to run the checks at startup that prevent a potentially unwanted release from
    /// happening. If this is set to `true`, the following checks will be disabled:
    ///
    /// * Preventing multiple releases on the channel the same day.
    /// * Preventing multiple releases on the channel of the same git commit.
    /// * Preventing multiple releases on stable and beta of the same version number.
    pub(crate) bypass_startup_checks: bool,

    /// Whether to force the recompression of .gz files into .xz.
    pub(crate) wip_recompress: bool,

    /// The compression level to use when recompressing tarballs with gzip.
    pub(crate) gzip_compression_level: u32,
    /// Custom sha of the commit to release, instead of the latest commit in the channel's branch.
    pub(crate) override_commit: Option<String>,
    /// Custom Endpoint URL for S3. Set this if you want to point to an S3-compatible service
    /// instead of the AWS one.
    pub(crate) s3_endpoint_url: Option<String>,
    /// Whether to skip invalidating the CloudFront distributions. This is useful when running the
    /// release process locally, without access to the production AWS account.
    pub(crate) skip_cloudfront_invalidations: bool,
}

impl Config {
    pub(crate) fn from_env() -> Result<Self, Error> {
        Ok(Self {
            bypass_startup_checks: bool_env("BYPASS_STARTUP_CHECKS")?,
            channel: require_env("CHANNEL")?,
            cloudfront_doc_id: require_env("CLOUDFRONT_DOC_ID")?,
            cloudfront_static_id: require_env("CLOUDFRONT_STATIC_ID")?,
            download_bucket: require_env("DOWNLOAD_BUCKET")?,
            download_dir: require_env("DOWNLOAD_DIR")?,
            gpg_key_file: require_env("GPG_KEY_FILE")?,
            gpg_password_file: require_env("GPG_PASSWORD_FILE")?,
            gzip_compression_level: default_env("GZIP_COMPRESSION_LEVEL", 9)?,
            num_threads: default_env("NUM_THREADS", num_cpus::get())?,
            override_commit: maybe_env("OVERRIDE_COMMIT")?,
            repository: default_env("REPOSITORY", "https://github.com/rust-lang/rust.git".into())?,
            repository_authentication: maybe_env("REPOSITORY_AUTHENTICATION")?,
            s3_endpoint_url: maybe_env("S3_ENDPOINT_URL")?,
            skip_cloudfront_invalidations: bool_env("SKIP_CLOUDFRONT_INVALIDATIONS")?,
            upload_addr: require_env("UPLOAD_ADDR")?,
            upload_bucket: require_env("UPLOAD_BUCKET")?,
            upload_dir: require_env("UPLOAD_DIR")?,
            wip_recompress: bool_env("WIP_RECOMPRESS")?,
        })
    }
}

pub(crate) struct RepositoryCredentials {
    username: String,
    password: String,
}

impl RepositoryCredentials {
    pub(crate) fn as_git2_credentials(&self) -> Result<Cred, git2::Error> {
        Ok(Cred::userpass_plaintext(&self.username, &self.password)?)
    }
}

impl FromStr for RepositoryCredentials {
    type Err = Error;

    fn from_str(input: &str) -> Result<Self, Error> {
        let (username, password) = input
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("username and password must be separated by a `:`"))?;
        Ok(Self {
            username: username.into(),
            password: password.into(),
        })
    }
}

fn maybe_env<R>(name: &str) -> Result<Option<R>, Error>
where
    R: FromStr,
    Error: From<R::Err>,
{
    match std::env::var(format!("{}{}", ENVIRONMENT_VARIABLE_PREFIX, name)) {
        Ok(val) => Ok(Some(val.parse().map_err(Error::from).context(format!(
            "the {} environment variable has invalid content",
            name
        ))?)),
        Err(VarError::NotPresent) => Ok(None),
        Err(VarError::NotUnicode(_)) => {
            anyhow::bail!("environment variable {} is not unicode!", name)
        }
    }
}

fn require_env<R>(name: &str) -> Result<R, Error>
where
    R: FromStr,
    Error: From<R::Err>,
{
    match maybe_env(name)? {
        Some(res) => Ok(res),
        None => anyhow::bail!("missing environment variable {}", name),
    }
}

fn default_env<R>(name: &str, default: R) -> Result<R, Error>
where
    R: FromStr,
    Error: From<R::Err>,
{
    Ok(maybe_env(name)?.unwrap_or(default))
}

fn bool_env(name: &str) -> Result<bool, Error> {
    Ok(maybe_env::<String>(name)?.is_some())
}

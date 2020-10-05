use anyhow::{Context, Error};
use std::env::VarError;
use std::str::FromStr;

const ENVIRONMENT_VARIABLE_PREFIX: &str = "PROMOTE_RELEASE_";

pub(crate) struct Config {
    /// The channel we're currently releasing.
    pub(crate) channel: String,
    /// CloudFront distribution ID for doc.rust-lang.org.
    pub(crate) cloudfront_doc_id: String,
    /// CloudFront distribution ID for static.rust-lang.org.
    pub(crate) cloudfront_static_id: String,
    /// The S3 bucket that CI artifacts will be downloaded from.
    pub(crate) download_bucket: String,
    /// The S3 directory that CI artifacts will be downloaded from.
    pub(crate) download_dir: String,
    /// Path of the file containing the password of the GPG secret key.
    pub(crate) gpg_password_file: String,
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

    /// Whether to allow multiple releases on the same channel in the same day or not.
    pub(crate) allow_multiple_today: bool,

    /// The compression level to use when recompressing tarballs with gzip.
    pub(crate) gzip_compression_level: u32,
    /// Custom name of the branch to start the release process from, instead of the default one.
    pub(crate) override_branch: Option<String>,
    /// Custom Endpoint URL for S3. Set this if you want to point to an S3-compatible service
    /// instead of the AWS one.
    pub(crate) s3_endpoint_url: Option<String>,
    /// Whether to skip invalidating the CloudFront distributions. This is useful when running the
    /// release process locally, without access to the production AWS account.
    pub(crate) skip_cloudfront_invalidations: bool,
    /// Whether to avoid deleting the Rust build dir or not. Deleting it will improve the execution
    /// time, but it will use more disk space.
    pub(crate) skip_delete_build_dir: bool,
}

impl Config {
    pub(crate) fn from_env() -> Result<Self, Error> {
        Ok(Self {
            allow_multiple_today: bool_env("ALLOW_MULTIPLE_TODAY")?,
            channel: require_env("CHANNEL")?,
            cloudfront_doc_id: require_env("CLOUDFRONT_DOC_ID")?,
            cloudfront_static_id: require_env("CLOUDFRONT_STATIC_ID")?,
            download_bucket: require_env("DOWNLOAD_BUCKET")?,
            download_dir: require_env("DOWNLOAD_DIR")?,
            gpg_password_file: require_env("GPG_PASSWORD_FILE")?,
            gzip_compression_level: default_env("GZIP_COMPRESSION_LEVEL", 9)?,
            override_branch: maybe_env("OVERRIDE_BRANCH")?,
            s3_endpoint_url: maybe_env("S3_ENDPOINT_URL")?,
            skip_cloudfront_invalidations: bool_env("SKIP_CLOUDFRONT_INVALIDATIONS")?,
            skip_delete_build_dir: bool_env("SKIP_DELETE_BUILD_DIR")?,
            upload_addr: require_env("UPLOAD_ADDR")?,
            upload_bucket: require_env("UPLOAD_BUCKET")?,
            upload_dir: require_env("UPLOAD_DIR")?,
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

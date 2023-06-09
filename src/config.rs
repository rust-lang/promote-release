use crate::discourse::Discourse;
use crate::fastly::Fastly;
use crate::github::Github;
use crate::Context;
use anyhow::{Context as _, Error};
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

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    /// This is the default action, what we'll do if the environment variable
    /// isn't set. It takes the configured channel and pushes artifacts into the
    /// appropriate buckets, taking care of other helper tasks along the way.
    PromoteRelease,

    /// This promotes the branches up a single release:
    ///
    /// Let $stable, $beta, $master be the tips of each branch (when starting).
    ///
    /// * Set stable to $beta.
    /// * Set beta to $master (ish, look for the version bump).
    /// * Create a rust-lang/cargo branch for the appropriate beta commit.
    /// * Post a PR against the newly created beta branch bump src/ci/channel to `beta`.
    PromoteBranches,
}

impl FromStr for Action {
    type Err = Error;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "promote-release" => Ok(Action::PromoteRelease),
            "promote-branches" => Ok(Action::PromoteBranches),
            _ => anyhow::bail!("unknown channel: {}", input),
        }
    }
}

pub(crate) struct Config {
    /// This is the action we're expecting to take.
    pub(crate) action: Action,

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
    /// The storage class artifacts are created in. Primarily used for testing
    /// (we default to INTELLIGENT_TIERING if not set).
    pub(crate) storage_class: String,
    /// The S3 directory that release artifacts will be uploaded to.
    pub(crate) upload_dir: String,
    /// Whether to run the checks at startup that prevent a potentially unwanted release from
    /// happening. If this is set to `true`, the following checks will be disabled:
    ///
    /// * Preventing multiple releases on the channel the same day.
    /// * Preventing multiple releases on the channel of the same git commit.
    /// * Preventing multiple releases on stable and beta of the same version number.
    pub(crate) bypass_startup_checks: bool,

    /// Whether to force the recompression from input tarballs into .gz compressed tarballs.
    ///
    /// This is on by default if .gz tarballs aren't available in the input.
    pub(crate) recompress_gz: bool,
    /// Whether to force the recompression from input tarballs into highly compressed .xz tarballs.
    pub(crate) recompress_xz: bool,

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

    /// Where to tag stable rustc releases.
    ///
    /// This repository should have content write permissions with the github
    /// app configuration.
    ///
    /// Should be a org/repo code, e.g., rust-lang/rust.
    pub(crate) rustc_tag_repository: Option<String>,

    /// Where to tag stable cargo releases.
    ///
    /// This repository should have content write permissions with the github
    /// app configuration.
    ///
    /// Should be a org/repo code, e.g., rust-lang/cargo.
    pub(crate) cargo_tag_repository: Option<String>,

    /// Where to publish new blog PRs.
    ///
    /// We create a new PR announcing releases in this repository; currently we
    /// don't automatically merge it (but that might change in the future).
    ///
    /// Should be a org/repo code, e.g., rust-lang/blog.rust-lang.org.
    pub(crate) blog_repository: Option<String>,

    /// This is the PR on the blog repository we should merge (using GitHub PR merge) after
    /// finishing this release.
    ///
    /// This is currently used for stable releases but in principle could be used for arbitrary
    /// releases.
    pub(crate) blog_pr: Option<u32>,

    /// The expected release date, for the blog post announcing dev-static
    /// releases. Expected to be in YYYY-MM-DD format.
    ///
    /// This is used to produce the expected release date in blog posts and to
    /// generate the release notes URL (targeting stable branch on
    /// rust-lang/rust).
    pub(crate) scheduled_release_date: Option<chrono::NaiveDate>,

    /// These are Discourse configurations for where to post dev-static
    /// announcements. Currently we only post dev release announcements.
    pub(crate) discourse_api_key: Option<String>,
    pub(crate) discourse_api_user: Option<String>,

    /// This is a github app private key, used for the release steps which
    /// require action on GitHub (e.g., kicking off a new thanks GHA build,
    /// opening pull requests against the blog for dev releases, promoting
    /// branches). Not all of this is implemented yet but it's all going to use
    /// tokens retrieved from the github app here.
    ///
    /// Currently this isn't really exercised in CI, but that might change in
    /// the future with a github app scoped to a 'fake' org or something like
    /// that.
    pub(crate) github_app_key: Option<String>,

    /// The app ID associated with the private key being passed.
    pub(crate) github_app_id: Option<u32>,

    /// An API token for Fastly with the `purge_select` scope.
    pub(crate) fastly_api_token: Option<String>,
    /// The static domain name that is used with Fastly, e.g. `static.rust-lang.org`.
    pub(crate) fastly_static_domain: Option<String>,

    /// Temporary variable to test Fastly in the dev environment only.
    pub(crate) invalidate_fastly: bool,
}

impl Config {
    pub(crate) fn from_env() -> Result<Self, Error> {
        Ok(Self {
            action: default_env("ACTION", Action::PromoteRelease)?,
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
            s3_endpoint_url: maybe_env("S3_ENDPOINT_URL")?,
            skip_cloudfront_invalidations: bool_env("SKIP_CLOUDFRONT_INVALIDATIONS")?,
            upload_addr: require_env("UPLOAD_ADDR")?,
            upload_bucket: require_env("UPLOAD_BUCKET")?,
            storage_class: default_env("UPLOAD_STORAGE_CLASS", "INTELLIGENT_TIERING".into())?,
            upload_dir: require_env("UPLOAD_DIR")?,
            recompress_xz: bool_env("RECOMPRESS_XZ")?,
            recompress_gz: bool_env("RECOMPRESS_GZ")?,
            rustc_tag_repository: maybe_env("RUSTC_TAG_REPOSITORY")?,
            cargo_tag_repository: maybe_env("CARGO_TAG_REPOSITORY")?,
            blog_repository: maybe_env("BLOG_REPOSITORY")?,
            blog_pr: maybe_env("BLOG_MERGE_PR")?,
            scheduled_release_date: maybe_env("BLOG_SCHEDULED_RELEASE_DATE")?,
            discourse_api_user: maybe_env("DISCOURSE_API_USER")?,
            discourse_api_key: maybe_env("DISCOURSE_API_KEY")?,
            github_app_key: maybe_env("GITHUB_APP_KEY")?,
            github_app_id: maybe_env("GITHUB_APP_ID")?,
            fastly_api_token: maybe_env("FASTLY_API_TOKEN")?,
            fastly_static_domain: maybe_env("FASTLY_STATIC_DOMAIN")?,
            invalidate_fastly: bool_env("INVALIDATE_FASTLY")?,
        })
    }

    pub(crate) fn github(&self) -> Option<Github> {
        if let (Some(key), Some(id)) = (&self.github_app_key, self.github_app_id) {
            Some(Github::new(key, id))
        } else {
            None
        }
    }
    pub(crate) fn discourse(&self) -> Option<Discourse> {
        if let (Some(key), Some(user)) = (&self.discourse_api_key, &self.discourse_api_user) {
            Some(Discourse::new(
                "https://internals.rust-lang.org".to_owned(),
                user.clone(),
                key.clone(),
            ))
        } else {
            None
        }
    }

    pub(crate) fn fastly(&self) -> Option<Fastly> {
        if let (Some(token), Some(domain)) = (&self.fastly_api_token, &self.fastly_static_domain) {
            Some(Fastly::new(token.clone(), domain.clone()))
        } else {
            None
        }
    }

    pub(crate) fn stable_dev_static_blog_contents(
        &self,
        release: &str,
        archive_date: &str,
        for_blog: bool,
        internals_url: Option<&str>,
    ) -> Option<String> {
        let scheduled_release_date = self.scheduled_release_date?;
        let release_notes_url = format!(
            "https://github.com/rust-lang/rust/blob/stable/RELEASES.md#version-{}-{}",
            release.replace('.', ""),
            scheduled_release_date.format("%Y-%m-%d"),
        );
        let human_date = scheduled_release_date.format("%B %-d");
        let internals = internals_url
            .map(|url| format!("You can leave feedback on the [internals thread]({url})."))
            .unwrap_or_default();
        let prefix = if for_blog {
            format!(
                r#"---
layout: post
title: "{} pre-release testing"
author: Release automation
team: The Release Team <https://www.rust-lang.org/governance/teams/release>
---{}"#,
                release, "\n\n",
            )
        } else {
            String::new()
        };
        Some(format!(
            "{prefix}The {release} pre-release is ready for testing. The release is scheduled for
{human_date}. [Release notes can be found here.][relnotes]

You can try it out locally by running:

```plain
RUSTUP_DIST_SERVER=https://dev-static.rust-lang.org rustup update stable
```

The index is <https://dev-static.rust-lang.org/dist/{archive_date}/index.html>.

{internals}

The release team is also thinking about changes to our pre-release process:
we'd love your feedback [on this GitHub issue][feedback].

[relnotes]: {release_notes_url}
[feedback]: https://github.com/rust-lang/release-team/issues/16
    "
        ))
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

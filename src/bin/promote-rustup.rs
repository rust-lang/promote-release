use anyhow::Context;
use promote_release::cloudfront::invalidate_cloudfront;
use promote_release::{Github, S3Client, require_env};
use secrecy::{ExposeSecret, SecretString};
use std::path::Path;
use std::str::FromStr;

enum Command {
    /// Promote nightly artifacts (the latest `main` commit) to the `dev-static` production bucket.
    Nightly,
}

impl FromStr for Command {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "nightly" => Ok(Self::Nightly),
            _ => Err(format!("Invalid command {s}")),
        }
    }
}

struct RustupConfig {
    /// This is a GitHub App private key used for invoking the GitHub API.
    github_app_key: SecretString,
    /// The app ID associated with the private key being passed.
    github_app_id: u32,
}
impl RustupConfig {
    fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            github_app_key: SecretString::new(
                require_env::<String>("GITHUB_APP_KEY")?.into_boxed_str(),
            ),
            github_app_id: require_env("GITHUB_APP_ID")?,
        })
    }
}

// Usage: promote-rustup <command>
fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let cmd: Command = args
        .next()
        .context("missing command")?
        .parse()
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("invalid command")?;

    let workdir = Path::new("rustup-work");
    let cfg = RustupConfig::from_env().context("Cannot load rustup context")?;
    match cmd {
        Command::Nightly => promote_nightly(&workdir, cfg),
    }
}

/// Get the latest commit from rustup's `main` branch and download its artifacts (which were
/// previously uploaded by rustup's CI) from S3.
/// Then re-upload those artifacts into the dev-static bucket, so that the dev.rustup.rs website
/// and dev-static rustup binaries are updated.
/// Also take care of invalidating Cloudfront.
fn promote_nightly(workdir: &Path, cfg: RustupConfig) -> anyhow::Result<()> {
    let mut github = Github::new(cfg.github_app_key.expose_secret(), cfg.github_app_id);
    let mut gh_client = github.unauthenticated("rust-lang/rustup");
    let head_sha = gh_client.get_ref("heads/main")?;
    eprintln!("SHA: {head_sha}");

    let s3_client = S3Client::new(None);

    // Download artifacts for the given commit SHA
    let download_dir = workdir.join("dl").join(&head_sha);
    let commit_bucket = format!("s3://rustup-builds/{head_sha}");
    s3_client
        .download(&commit_bucket, &download_dir)
        .context("Cannot download rustup build")?;

    // Update dev.rustup.rs
    let www_dir = download_dir.join("www");

    let dev_static_bucket = "dev-static-rust-lang-org";
    s3_client
        .upload(&www_dir, &format!("s3://{dev_static_bucket}/rustup/www"))
        .context("Cannot upload www files")?;

    invalidate_cloudfront(&workdir, "E30AO2GXMDY230", &["/rustup/*".to_string()])
        .context("Cannot invalidate nightly Rustup")?;
    // Invalidate dev.rustup.rs
    invalidate_cloudfront(&workdir, "E3OQOQ34607Z0A", &["/*".to_string()])
        .context("Cannot invalidate dev.rustup.rs")?;

    Ok(())
}

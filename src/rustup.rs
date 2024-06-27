use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context as AnyhowContext, Error};
use serde::Deserialize;

use crate::config::Channel;
use crate::{run, Context};

impl Context {
    /// Promote a `rustup` release
    ///
    /// The [release process] for `rustup` involves copying existing artifacts from one S3 bucket to
    /// another, updating the manifest, and archiving the artifacts for long-term storage.
    ///
    /// `rustup` uses different branches to manage releases. Whenever a commit is pushed to the
    /// `stable` branch in [rust-lang/rustup], GitHub Actions workflows build release artifacts and
    /// copy them into `s3://dev-static-rust-lang-org/rustup/dist/`.
    ///
    /// When a new release is done and this method is invoked, it downloads the artifacts from that
    /// bucket (which must always be set as the `DOWNLOAD_BUCKET` variable). A copy of the artifacts
    /// is archived in `s3://${UPLOAD_BUCKET}/rustup/archive/${version}/`, where `version` is passed
    /// to this program as a command-line argument. `UPLOAD_BUCKET` can either be the `dev-static`
    /// or the `static` bucket.
    ///
    /// If the release is for the `stable` channel, the artifacts are also copied to the `dist/`
    /// path in the `UPLOAD_BUCKET` bucket. The `dist/` path is used by the `rustup` installer to
    /// download the latest release.
    ///
    /// Then, the `release-stable.toml` manifest is updated with the new version and copied to
    /// `s3://${UPLOAD_BUCKET}/rustup/release-stable.toml`.
    ///
    /// [release process]: https://rust-lang.github.io/rustup/dev-guide/release-process.html
    /// [rust-lang/rustup]: https://github.com/rust-lang/rustup
    pub fn promote_rustup(&mut self) -> anyhow::Result<()> {
        // Rustup only has beta and stable releases, so we fail fast when trying to promote nightly
        self.enforce_rustup_channel()?;

        // The latest commit on the `stable` branch is used to determine the version number
        let head_sha = self.get_head_sha_for_rustup()?;
        let version = self.get_next_rustup_version(&head_sha)?;

        // Download the rustup artifacts from S3
        println!("Downloading artifacts from dev-static...");
        let dist_dir = self.download_rustup_artifacts()?;

        // Archive the artifacts
        println!("Archiving artifacts...");
        self.archive_rustup_artifacts(&dist_dir)?;

        if self.config.channel == Channel::Stable {
            // Promote the artifacts to the release bucket
            println!("Promoting artifacts to dist/...");
            self.promote_rustup_artifacts(&dist_dir)?;
        }

        // Update the release number
        println!("Updating version and manifest...");
        self.update_rustup_release()?;

        Ok(())
    }

    fn enforce_rustup_channel(&self) -> anyhow::Result<()> {
        println!("Checking channel...");

        if self.config.channel != Channel::Stable && self.config.channel != Channel::Beta {
            return Err(anyhow!(
                "promoting rustup is only supported for the stable and beta channels"
            ));
        }

        Ok(())
    }

    fn get_head_sha_for_rustup(&self) -> anyhow::Result<String> {
        self.config
            .github()
            .context("failed to get HEAD SHA from GitHub - credentials not configured")?
            .token("rust-lang/rustup")?
            .get_ref("heads/stable")
    }

    fn get_next_rustup_version(&self, sha: &str) -> anyhow::Result<String> {
        println!("Getting next Rustup version from Cargo.toml...");

        #[derive(Debug, Deserialize)]
        struct CargoToml {
            version: String,
        }

        let cargo_toml = self
            .config
            .github()
            .context("failed to get new rustup version from GitHub - credentials not configured")?
            .token("rust-lang/rustup")?
            .read_file(Some(sha), "Cargo.toml")?;

        let toml: CargoToml = toml::from_str(&cargo_toml.content()?)?;

        Ok(toml.version)
    }

    fn download_rustup_artifacts(&mut self) -> Result<PathBuf, Error> {
        let dl = self.dl_dir().join("dist");
        // Remove the directory if it exists, otherwise just ignore.
        let _ = fs::remove_dir_all(&dl);
        fs::create_dir_all(&dl)?;

        run(self
            .aws_s3()
            .arg("cp")
            .arg("--recursive")
            .arg("--only-show-errors")
            .arg(&self.s3_artifacts_url("dist/"))
            .arg(format!("{}/", dl.display())))?;

        Ok(dl)
    }

    fn archive_rustup_artifacts(&mut self, dist_dir: &Path) -> Result<(), Error> {
        let version = self
            .current_version
            .as_ref()
            .ok_or_else(|| anyhow!("failed to get current version for rustup release"))?;

        let path = format!("archive/{}/", version);

        self.upload_rustup_artifacts(dist_dir, &path)
    }

    fn promote_rustup_artifacts(&mut self, dist_dir: &Path) -> Result<(), Error> {
        let release_bucket_url = format!(
            "s3://{}/{}/{}",
            self.config.upload_bucket,
            self.config.download_dir,
            dist_dir.display(),
        );

        run(self
            .aws_s3()
            .arg("cp")
            .arg("--recursive")
            .arg("--only-show-errors")
            .arg(format!("{}/", dist_dir.display()))
            .arg(&release_bucket_url))
    }

    fn upload_rustup_artifacts(&mut self, dist_dir: &Path, target_path: &str) -> Result<(), Error> {
        run(self
            .aws_s3()
            .arg("cp")
            .arg("--recursive")
            .arg("--only-show-errors")
            .arg(format!("{}/", dist_dir.display()))
            .arg(&self.s3_artifacts_url(target_path)))
    }

    fn update_rustup_release(&mut self) -> Result<(), Error> {
        let version = self
            .current_version
            .as_ref()
            .ok_or_else(|| anyhow!("failed to get current version for rustup release"))?;

        let manifest_path = self.dl_dir().join("release-stable.toml");
        let manifest = format!(
            r#"
schema-version = '1'
version = '{}'
            "#,
            version
        );

        fs::write(&manifest_path, manifest)?;

        run(self
            .aws_s3()
            .arg("cp")
            .arg("--only-show-errors")
            .arg(manifest_path)
            .arg(&self.s3_artifacts_url("release-stable.toml")))
    }
}

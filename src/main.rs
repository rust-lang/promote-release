mod config;

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::Error;
use curl::easy::Easy;
use fs2::FileExt;
use rayon::prelude::*;

use crate::config::Config;

struct Context {
    work: PathBuf,
    release: String,
    handle: Easy,
    secrets: SecretsDist,
    config: Config,
    date: String,
    current_version: Option<String>,
}

// Called as:
//
//  $prog work/dir release-channel path/to/secrets.toml
fn main() -> Result<(), Error> {
    let secrets = std::fs::read_to_string(env::args().nth(3).unwrap())?;

    Context {
        work: env::current_dir()?.join(env::args_os().nth(1).unwrap()),
        release: env::args().nth(2).unwrap(),
        secrets: toml::from_str::<Secrets>(&secrets)?.dist,
        config: Config::from_env()?,
        handle: Easy::new(),
        date: output(Command::new("date").arg("+%Y-%m-%d"))?
            .trim()
            .to_string(),
        current_version: None,
    }
    .run()
}

impl Context {
    fn run(&mut self) -> Result<(), Error> {
        let _lock = self.lock()?;
        self.update_repo()?;

        let branch = if let Some(branch) = self.config.override_branch.clone() {
            branch
        } else {
            match &self.release[..] {
                "nightly" => "master",
                "beta" => "beta",
                "stable" => "stable",
                _ => panic!("unknown release: {}", self.release),
            }
            .to_string()
        };
        self.do_release(&branch)?;

        Ok(())
    }

    /// Locks execution of concurrent invocations of this script in case one
    /// takes a long time to run. The call to `try_lock_exclusive` will fail if
    /// the lock is held already
    fn lock(&mut self) -> Result<File, Error> {
        fs::create_dir_all(&self.work)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(self.work.join(".lock"))?;
        file.try_lock_exclusive()?;
        Ok(file)
    }

    /// Update the rust repository we have cached, either cloning a fresh one or
    /// fetching remote references
    fn update_repo(&mut self) -> Result<(), Error> {
        // Clone/update the repo
        let dir = self.rust_dir();
        if dir.is_dir() {
            println!("fetching");
            run(Command::new("git")
                .arg("fetch")
                .arg("origin")
                .current_dir(&dir))?;
        } else {
            println!("cloning");
            run(Command::new("git")
                .arg("clone")
                .arg("https://github.com/rust-lang/rust")
                .arg(&dir))?;
        }

        Ok(())
    }

    /// Does a release for the `branch` specified.
    fn do_release(&mut self, branch: &str) -> Result<(), Error> {
        // Learn the precise rev of the remote branch, this'll guide what we
        // download.
        let rev = output(
            Command::new("git")
                .arg("rev-parse")
                .arg(format!("origin/{}", branch))
                .current_dir(&self.rust_dir()),
        )?;
        let rev = rev.trim();
        println!("{} rev is {}", self.release, rev);

        // Download the current live manifest for the channel we're releasing.
        // Through that we learn the current version of the release.
        let manifest = self.download_manifest()?;
        let previous_version = manifest["pkg"]["rust"]["version"]
            .as_str()
            .expect("rust version not a string");
        println!("previous version: {}", previous_version);

        // If the previously released version is the same rev, then there's
        // nothing for us to do, nothing has changed.
        if previous_version.contains(&rev[..7]) {
            println!("found rev in previous version, skipping");
            return Ok(());
        }

        // We may still not do a release if the version number hasn't changed.
        // To learn about the current branch's version number we download
        // artifacts and look inside.
        //
        // If revisions of the current release and the current branch are
        // different and the versions are the same then there's nothing for us
        // to do. This represents a scenario where changes have been merged to
        // the stable/beta branch but the version bump hasn't happened yet.
        self.download_artifacts(&rev)?;
        if self.current_version_same(&previous_version)? {
            println!("version hasn't changed, skipping");
            return Ok(());
        }

        self.assert_all_components_present()?;

        // Ok we've now determined that a release needs to be done. Let's
        // configure rust, build a manifest and sign the artifacts we just downloaded, and upload the
        // signatures and manifest to the CI bucket.
        self.configure_rust(rev)?;
        self.sign_artifacts()?;
        self.upload_signatures(&rev)?;

        // Merge all the signatures with the download files, and then sync that
        // whole dir up to the release archives
        for file in self.build_dir().join("build/dist/").read_dir()? {
            let file = file?;
            fs::copy(file.path(), self.dl_dir().join(file.file_name()))?;
        }
        self.publish_archive()?;
        self.publish_docs()?;
        self.publish_release()?;

        self.invalidate_releases()?;

        // Clean up after ourselves to avoid leaving gigabytes of artifacts
        // around.
        let _ = fs::remove_dir_all(&self.dl_dir());

        Ok(())
    }

    fn configure_rust(&mut self, rev: &str) -> Result<(), Error> {
        let build = self.build_dir();
        // Avoid deleting the build directory with the cached build artifacts when working locally.
        if !self.config.skip_delete_build_dir {
            let _ = fs::remove_dir_all(&build);
        }
        if !build.exists() {
            fs::create_dir_all(&build)?;
        }
        let rust = self.rust_dir();

        run(Command::new("git")
            .arg("reset")
            .arg("--hard")
            .arg(rev)
            .current_dir(&rust))?;

        run(Command::new(rust.join("configure"))
            .current_dir(&build)
            .arg(format!("--release-channel={}", self.release)))?;
        let mut config = String::new();
        let path = build.join("config.toml");
        drop(File::open(&path).and_then(|mut f| f.read_to_string(&mut config)));
        let lines = config.lines().filter(|l| !l.starts_with("[dist]"));
        let mut new_config = String::new();
        for line in lines {
            new_config.push_str(line);
            new_config.push_str("\n");
        }
        new_config.push_str(&format!(
            "
[dist]
sign-folder = \"{}\"
gpg-password-file = \"{}\"
upload-addr = \"{}/{}\"
",
            self.dl_dir().display(),
            self.secrets.gpg_password_file,
            self.secrets.upload_addr,
            self.secrets.upload_dir,
        ));
        std::fs::write(&path, new_config.as_bytes())?;

        Ok(())
    }

    fn current_version_same(&mut self, prev: &str) -> Result<bool, Error> {
        // nightly's always changing
        if self.release == "nightly" {
            return Ok(false);
        }
        let prev_version = prev.split(' ').next().unwrap();

        let mut current = None;
        for e in self.dl_dir().read_dir()? {
            let e = e?;
            let filename = e.file_name().into_string().unwrap();
            if !filename.starts_with("rustc-") || !filename.ends_with(".tar.gz") {
                continue;
            }
            println!("looking inside {} for a version", filename);

            let file = File::open(&e.path())?;
            let reader = flate2::read::GzDecoder::new(file);
            let mut archive = tar::Archive::new(reader);

            let mut version_file = None;
            for entry in archive.entries()? {
                let entry = entry?;
                let path = entry.path()?;
                if let Some(path) = path.iter().nth(1) {
                    if path == Path::new("version") {
                        version_file = Some(entry);
                        break;
                    }
                }
            }
            if let Some(mut entry) = version_file {
                let mut contents = String::new();
                entry.read_to_string(&mut contents)?;
                current = Some(contents);

                break;
            }
        }
        let current = current.ok_or_else(|| anyhow::anyhow!("no archives with a version"))?;

        println!("current version: {}", current);

        let current_version = current.split(' ').next().unwrap();
        self.current_version = Some(current_version.to_string());

        // The release process for beta looks like so:
        //
        // * Force push master branch to beta branch
        // * Send a PR to beta, updating release channel
        //
        // In the window between these two steps we don't actually have release
        // artifacts but this script may be run. Try to detect that case here if
        // the versions mismatch and panic. We'll try again later once that PR
        // has merged and everything should look good.
        if (current.contains("nightly") && !prev.contains("nightly"))
            || (current.contains("beta") && !prev.contains("beta"))
        {
            panic!(
                "looks like channels are being switched -- was this branch \
                    just created and has a pending PR to change the release \
                    channel?"
            );
        }

        Ok(prev_version == current_version)
    }

    /// Make sure this release comes with a minimum of components.
    ///
    /// Note that we already don't merge PRs in rust-lang/rust that don't
    /// build cargo, so this cannot realistically fail.
    fn assert_all_components_present(&self) -> Result<(), Error> {
        if self.release != "nightly" {
            return Ok(());
        }

        let mut components = Vec::new();
        for entry in self.dl_dir().read_dir()? {
            let name = entry?.file_name().into_string().unwrap();
            if name.contains("x86_64-unknown-linux-gnu") {
                components.push(name);
            }
        }

        println!("components in this nightly {:?}", components);
        assert!(components.iter().any(|s| s.starts_with("rustc-")));
        assert!(components.iter().any(|s| s.starts_with("rust-std-")));
        assert!(components.iter().any(|s| s.starts_with("cargo-")));
        // For now, produce nightlies even if these are missing.
        // assert!(components.iter().any(|s| s.starts_with("rustfmt-")));
        // assert!(components.iter().any(|s| s.starts_with("rls-")));
        // assert!(components.iter().any(|s| s.starts_with("clippy-")));

        Ok(())
    }

    fn download_artifacts(&mut self, rev: &str) -> Result<(), Error> {
        let dl = self.dl_dir();
        let _ = fs::remove_dir_all(&dl);
        fs::create_dir_all(&dl)?;

        run(self
            .aws_s3()
            .arg("cp")
            .arg("--recursive")
            .arg("--only-show-errors")
            .arg(&self.s3_artifacts_url(&format!("{}/", rev)))
            .arg(format!("{}/", dl.display())))?;

        let mut files = dl.read_dir()?;
        if files.next().is_none() {
            panic!(
                "appears that this rev doesn't have any artifacts, \
                    is this a stable/beta branch awaiting a PR?"
            );
        }

        // Delete residue signature/hash files. These may come around for a few
        // reasons:
        //
        // 1. We died halfway through before uploading the manifest, in which
        //    case we want to re-upload everything but we don't want to sign
        //    signatures.
        //
        // 2. We're making a stable release. The stable release is first signed
        //    with the dev key and then it's signed with the prod key later. We
        //    want the prod key to overwrite the dev key signatures.
        //
        // Also, collect paths that need to be recompressed
        let mut to_recompress = Vec::new();
        for file in dl.read_dir()? {
            let file = file?;
            let path = file.path();
            match path.extension().and_then(|s| s.to_str()) {
                // Delete signature/hash files...
                Some("asc") | Some("sha256") => {
                    fs::remove_file(&path)?;
                }
                // Generate *.gz from *.xz...
                Some("xz") => {
                    let gz_path = path.with_extension("gz");
                    if !gz_path.is_file() {
                        to_recompress.push((path.to_path_buf(), gz_path));
                    }
                }
                _ => {}
            }
        }

        // Also, generate *.gz from *.xz if the former is missing. Since the gz
        // and xz tarballs have the same content, we did not deploy the gz files
        // from the CI. But rustup users may still expect to get gz files, so we
        // are recompressing the xz files as gz here.
        if !to_recompress.is_empty() {
            println!(
                "starting to recompress {} files across {} threads",
                to_recompress.len(),
                to_recompress.len().min(rayon::current_num_threads()),
            );
            let recompress_start = Instant::now();

            to_recompress
                .par_iter()
                .map(|(xz_path, gz_path)| {
                    println!("recompressing {}...", gz_path.display());

                    let xz = File::open(xz_path)?;
                    let mut xz = xz2::read::XzDecoder::new(xz);
                    let gz = File::create(gz_path)?;
                    let mut gz = flate2::write::GzEncoder::new(gz, flate2::Compression::best());
                    io::copy(&mut xz, &mut gz)?;

                    Ok::<(), Error>(())
                })
                .collect::<Result<Vec<()>, Error>>()?;

            println!(
                "finished recompressing {} files in {:.2?}",
                to_recompress.len(),
                recompress_start.elapsed(),
            );
        }

        Ok(())
    }

    /// Create manifest and sign the artifacts.
    fn sign_artifacts(&mut self) -> Result<(), Error> {
        let build = self.build_dir();
        // This calls `src/tools/build-manifest` from the rustc repo.
        run(Command::new(self.rust_dir().join("x.py"))
            .current_dir(&build)
            .arg("dist")
            .arg("hash-and-sign"))
    }

    fn upload_signatures(&mut self, rev: &str) -> Result<(), Error> {
        run(self
            .aws_s3()
            .arg("cp")
            .arg("--recursive")
            .arg("--only-show-errors")
            .arg(self.build_dir().join("build/dist/"))
            .arg(&self.s3_artifacts_url(&format!("{}/", rev))))
    }

    fn publish_archive(&mut self) -> Result<(), Error> {
        let bucket = &self.secrets.upload_bucket;
        let dir = &self.secrets.upload_dir;
        let dst = format!("s3://{}/{}/{}/", bucket, dir, self.date);
        run(self
            .aws_s3()
            .arg("cp")
            .arg("--recursive")
            .arg("--only-show-errors")
            .arg("--metadata-directive")
            .arg("REPLACE")
            .arg("--cache-control")
            .arg("public")
            .arg(format!("{}/", self.dl_dir().display()))
            .arg(&dst))
    }

    fn publish_docs(&mut self) -> Result<(), Error> {
        let (version, upload_dir) = match &self.release[..] {
            "stable" => {
                let vers = &self.current_version.as_ref().unwrap()[..];
                (vers, "stable")
            }
            "beta" => ("beta", "beta"),
            "nightly" => ("nightly", "nightly"),
            _ => panic!(),
        };

        // Pull out HTML documentation from one of the `rust-docs-*` tarballs.
        // For now we just arbitrarily pick x86_64-unknown-linux-gnu.
        let docs = self.work.join("docs");
        drop(fs::remove_dir_all(&docs));
        fs::create_dir_all(&docs)?;
        let target = "x86_64-unknown-linux-gnu";

        // Unpack the regular documentation tarball.
        let tarball_prefix = format!("rust-docs-{}-{}", version, target);
        let tarball = format!("{}.tar.gz", self.dl_dir().join(&tarball_prefix).display());
        let tarball_dir = format!("{}/rust-docs/share/doc/rust/html", tarball_prefix);
        run(Command::new("tar")
            .arg("xf")
            .arg(&tarball)
            .arg("--strip-components=6")
            .arg(&tarball_dir)
            .current_dir(&docs))?;

        // Construct path to rustc documentation.
        let tarball_prefix = format!("rustc-docs-{}-{}", version, target);
        let tarball = format!("{}.tar.gz", self.dl_dir().join(&tarball_prefix).display());

        // Only create and unpack rustc docs if artefacts include tarball.
        if Path::new(&tarball).exists() {
            let rustc_docs = docs.join("nightly-rustc");
            fs::create_dir_all(&rustc_docs)?;

            // Construct the path that contains the documentation inside the tarball.
            let tarball_dir = format!("{}/rustc-docs/share/doc/rust/html", tarball_prefix);
            let tarball_dir_new = format!("{}/rustc", tarball_dir);

            if Command::new("tar")
                .arg("tf")
                .arg(&tarball)
                .arg(&tarball_dir_new)
                .current_dir(&rustc_docs)
                .output()?
                .status
                .success()
            {
                // Unpack the rustc documentation into the new directory.
                run(Command::new("tar")
                    .arg("xf")
                    .arg(&tarball)
                    .arg("--strip-components=7")
                    .arg(&tarball_dir_new)
                    .current_dir(&rustc_docs))?;
            } else {
                // Unpack the rustc documentation into the new directory.
                run(Command::new("tar")
                    .arg("xf")
                    .arg(&tarball)
                    .arg("--strip-components=6")
                    .arg(&tarball_dir)
                    .current_dir(&rustc_docs))?;
            }
        }

        // Upload this to `/doc/$channel`
        let bucket = &self.secrets.upload_bucket;
        let dst = format!("s3://{}/doc/{}/", bucket, upload_dir);
        run(self
            .aws_s3()
            .arg("sync")
            .arg("--delete")
            .arg("--only-show-errors")
            .arg(format!("{}/", docs.display()))
            .arg(&dst))?;
        self.invalidate_docs(upload_dir)?;

        // Stable artifacts also go to `/doc/$version/
        if upload_dir == "stable" {
            let dst = format!("s3://{}/doc/{}/", bucket, version);
            run(self
                .aws_s3()
                .arg("sync")
                .arg("--delete")
                .arg("--only-show-errors")
                .arg(format!("{}/", docs.display()))
                .arg(&dst))?;
            self.invalidate_docs(&version)?;
        }

        Ok(())
    }

    fn invalidate_docs(&self, dir: &str) -> Result<(), Error> {
        self.invalidate_cloudfront(
            &self.secrets.rustdoc_cf_distribution_id,
            &[if dir == "stable" {
                "/*".into()
            } else {
                format!("/{}/*", dir)
            }],
        )
    }

    fn publish_release(&mut self) -> Result<(), Error> {
        let bucket = &self.secrets.upload_bucket;
        let dir = &self.secrets.upload_dir;
        let dst = format!("s3://{}/{}/", bucket, dir);
        run(self
            .aws_s3()
            .arg("cp")
            .arg("--recursive")
            .arg("--only-show-errors")
            .arg(format!("{}/", self.dl_dir().display()))
            .arg(&dst))
    }

    fn invalidate_releases(&self) -> Result<(), Error> {
        self.invalidate_cloudfront(
            &self.secrets.cloudfront_distribution_id,
            &[
                "/dist/channel*".into(),
                "/dist/rust*".into(),
                "/dist/index*".into(),
                "/dist/".into(),
            ],
        )
    }

    fn invalidate_cloudfront(&self, distribution_id: &str, paths: &[String]) -> Result<(), Error> {
        if self.config.skip_cloudfront_invalidations {
            println!();
            println!("WARNING! Skipped CloudFront invalidation of: {:?}", paths);
            println!("Unset PROMOTE_RELEASE_SKIP_CLOUDFRONT_INVALIDATIONS if you're in production");
            println!();
            return Ok(());
        }

        let json = serde_json::json!({
            "Paths": {
                "Items": paths,
                "Quantity": paths.len(),
            },
            "CallerReference": format!("rct-{}", rand::random::<usize>()),
        })
        .to_string();
        let dst = self.work.join("payload.json");
        std::fs::write(&dst, json.as_bytes())?;

        let mut cmd = Command::new("aws");
        self.aws_creds(&mut cmd);
        run(cmd
            .arg("cloudfront")
            .arg("create-invalidation")
            .arg("--invalidation-batch")
            .arg(format!("file://{}", dst.display()))
            .arg("--distribution-id")
            .arg(distribution_id))?;

        Ok(())
    }

    fn rust_dir(&self) -> PathBuf {
        self.work.join("rust")
    }

    fn dl_dir(&self) -> PathBuf {
        self.work.join("dl")
    }

    fn build_dir(&self) -> PathBuf {
        self.work.join("build")
    }

    fn s3_artifacts_url(&self, path: &str) -> String {
        format!(
            "s3://{}/{}/{}",
            self.secrets
                .download_bucket
                .as_deref()
                .unwrap_or("rust-lang-ci2"),
            self.secrets
                .download_dir
                .as_deref()
                .unwrap_or("rustc-builds"),
            path,
        )
    }

    fn aws_s3(&self) -> Command {
        let mut cmd = Command::new("aws");

        // Allow using non-S3 backends with the AWS CLI.
        if let Some(url) = &self.secrets.aws_s3_endpoint_url {
            cmd.arg("--endpoint-url");
            cmd.arg(url);
        }

        cmd.arg("s3");
        self.aws_creds(&mut cmd);
        cmd
    }

    fn aws_creds(&self, cmd: &mut Command) {
        cmd.env("AWS_ACCESS_KEY_ID", &self.secrets.aws_access_key_id)
            .env("AWS_SECRET_ACCESS_KEY", &self.secrets.aws_secret_key);
    }

    fn download_manifest(&mut self) -> Result<toml::Value, Error> {
        self.handle.get(true)?;
        let addr = &self.secrets.upload_addr;
        let upload_dir = &self.secrets.upload_dir;
        let url = format!("{}/{}/channel-rust-{}.toml", addr, upload_dir, self.release);
        println!("downloading manifest from: {}", url);
        self.handle.url(&url)?;
        let mut result = Vec::new();
        {
            let mut t = self.handle.transfer();

            t.write_function(|data| {
                result.extend_from_slice(data);
                Ok(data.len())
            })?;
            t.perform()?;
        }
        assert_eq!(self.handle.response_code()?, 200);
        Ok(String::from_utf8(result)?.parse()?)
    }
}

fn run(cmd: &mut Command) -> Result<(), Error> {
    println!("running {:?}", cmd);
    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("failed command:{:?}\n:{}", cmd, status);
    }
    Ok(())
}

fn output(cmd: &mut Command) -> Result<String, Error> {
    println!("running {:?}", cmd);
    let output = cmd.output()?;
    if !output.status.success() {
        anyhow::bail!(
            "failed command:{:?}\n:{}\n\n{}\n\n{}",
            cmd,
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    Ok(String::from_utf8(output.stdout)?)
}

#[derive(serde::Deserialize)]
struct Secrets {
    dist: SecretsDist,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
struct SecretsDist {
    /// Path to the file containing the password of the gpg key.
    gpg_password_file: String,

    /// Remote HTTP host artifacts will be uploaded to. Note that this is *not* the same as what's
    /// configured in `config.toml` for rustbuild, it's just the *host* that we're uploading to and
    /// going to be looking at urls from.
    ///
    /// This is used in a number of places such as:
    ///
    /// * Downloading manifestss
    /// * Urls in manifests
    ///
    /// and possibly more. Note that most urls end up appending `upload-dir` below to this address
    /// specified. This address should not have a trailing slash.
    upload_addr: String,

    /// The S3 bucket that release artifacts will be uploaded to.
    upload_bucket: String,
    /// The S3 directory that release artifacts will be uploaded to.
    upload_dir: String,

    /// The S3 bucket that CI artifacts will be downloaded from.
    download_bucket: Option<String>,
    /// The S3 directory that CI artifacts will be downloaded from.
    download_dir: Option<String>,

    /// Credentials for S3 downloads/uploads. As of this writing the credentials need
    /// to have permissions to:
    ///
    /// * Upload/download/list to the "download" bucket specified above
    /// * Upload/download/list to the "upload" bucket specified above
    /// * Create a cloudfront invalidation of the IDs below
    aws_access_key_id: String,
    /// Secret key of the access key specified above
    aws_secret_key: String,

    /// Custom Endpoint URL for S3. Set this if you want to point to an S3-compatible service
    /// instead of the AWS one.
    aws_s3_endpoint_url: Option<String>,

    /// CloudFront Distribution ID for static.rust-lang.org
    cloudfront_distribution_id: String,
    /// CloudFront Distribution ID for doc.rust-lang.org
    rustdoc_cf_distribution_id: String,
}

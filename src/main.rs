mod build_manifest;
mod config;
mod sign;
mod smoke_test;

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;
use std::{collections::HashSet, env};

use crate::build_manifest::BuildManifest;
use crate::sign::Signer;
use crate::smoke_test::SmokeTester;
use anyhow::Error;
use chrono::Utc;
use curl::easy::Easy;
use fs2::FileExt;
use rayon::prelude::*;
use xz2::read::XzDecoder;

use crate::config::{Channel, Config};

const TARGET: &str = env!("TARGET");

struct Context {
    work: PathBuf,
    handle: Easy,
    config: Config,
    date: String,
    current_version: Option<String>,
}

// Called as:
//
//  $prog work/dir
fn main() -> Result<(), Error> {
    let mut context = Context::new(
        env::current_dir()?.join(env::args_os().nth(1).unwrap()),
        Config::from_env()?,
    )?;
    context.run()
}

impl Context {
    fn new(work: PathBuf, config: Config) -> Result<Self, Error> {
        let date = Utc::now().format("%Y-%m-%d").to_string();

        // Configure the right amount of Rayon threads.
        rayon::ThreadPoolBuilder::new()
            .num_threads(config.num_threads)
            .build_global()?;

        Ok(Context {
            work,
            config,
            date,
            handle: Easy::new(),
            current_version: None,
        })
    }

    fn run(&mut self) -> Result<(), Error> {
        let _lock = self.lock()?;
        self.do_release()?;

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
    fn legacy_update_repo(&mut self) -> Result<(), Error> {
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

    fn get_commit_sha(&self) -> Result<String, Error> {
        if let Some(commit) = self.config.override_commit.clone() {
            return Ok(commit);
        }

        let git_ref = match self.config.channel {
            Channel::Nightly => "refs/heads/master",
            Channel::Beta => "refs/heads/beta",
            Channel::Stable => "refs/heads/stable",
        };

        // git2 requires a git repository to be able to connect to a remote and fetch metadata, so
        // this creates an empty repository in a temporary directory. It will be deleted once the
        // function returns.
        let temp = tempfile::tempdir()?;
        let repo = git2::Repository::init(temp.path())?;

        let mut remote = repo.remote("origin", &self.config.repository)?;
        remote.connect(git2::Direction::Fetch)?;

        for head in remote.list()? {
            if head.name() == git_ref {
                return Ok(hex::encode(head.oid().as_bytes()));
            }
        }
        anyhow::bail!("missing git ref in {}: {}", self.config.repository, git_ref);
    }

    fn do_release(&mut self) -> Result<(), Error> {
        let rev = self.get_commit_sha()?;
        println!("{} rev is {}", self.config.channel, rev);

        // Download the current live manifest for the channel we're releasing.
        // Through that we learn the current version of the release.
        let manifest = self.download_top_level_manifest()?;
        let previous_version = manifest["pkg"]["rust"]["version"]
            .as_str()
            .expect("rust version not a string");
        println!("previous version: {}", previous_version);

        // If the previously released version is the same rev, then there's
        // nothing for us to do, nothing has changed.
        if !self.config.bypass_startup_checks && previous_version.contains(&rev[..7]) {
            println!("found rev in previous version, skipping");
            println!("set PROMOTE_RELEASE_BYPASS_STARTUP_CHECKS=1 to bypass the check");
            return Ok(());
        }

        // During normal operations we don't want multiple releases to happen on the same channel
        // in the same day. This check prevents that, and it can be skipped by setting an
        // environment variable if the person doing the release really wants that.
        if !self.config.bypass_startup_checks && self.dated_manifest_exists()? {
            println!(
                "another release on the {} channel was done today ({}), skipping",
                self.config.channel, self.date
            );
            println!("set PROMOTE_RELEASE_BYPASS_STARTUP_CHECKS=1 to bypass the check");
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
        // The bypass_startup_checks condition is after the function call since we need that
        // function to run even if we wan to discard its output (it fetches and stores the current
        // version we're about to release).
        if self.current_version_same(&previous_version)? && !self.config.bypass_startup_checks {
            println!("version hasn't changed, skipping");
            println!("set PROMOTE_RELEASE_BYPASS_STARTUP_CHECKS=1 to bypass the check");
            return Ok(());
        }

        self.assert_all_components_present()?;

        // Ok we've now determined that a release needs to be done.

        let mut signer = Signer::new(&self.config)?;
        let build_manifest = BuildManifest::new(self);

        if build_manifest.exists() {
            let smoke_test = SmokeTester::new(&[self.manifest_dir(), self.dl_dir()])?;

            // First of all, a manifest is generated pointing to the smoke test server. This will
            // produce the correct checksums and shipped files list, as the only difference from
            // between the "real" execution and this one is the URLs included in the manifest.
            let execution =
                build_manifest.run(&format!("http://{}/dist", smoke_test.server_addr()))?;
            signer.override_checksum_cache(execution.checksum_cache);

            if self.config.wip_prune_unused_files {
                // Removes files that we are not shipping from the files we're about to upload.
                if let Some(shipped_files) = &execution.shipped_files {
                    self.prune_unused_files(&shipped_files)?;
                }
            }

            // Sign both the downloaded artifacts and the generated manifests. The signatures of
            // the downloaded files are permanent, while the signatures for the generated manifests
            // will be discarded later (as the manifests point to the smoke test server).
            signer.sign_directory(&self.dl_dir())?;
            signer.sign_directory(&self.manifest_dir())?;

            // Ensure the release is downloadable from rustup and can execute a basic binary.
            smoke_test.test(&self.config.channel)?;

            // Generate the real manifests and sign them.
            build_manifest.run(&format!(
                "{}/{}",
                self.config.upload_addr, self.config.upload_dir
            ))?;
            signer.sign_directory(&self.manifest_dir())?;

            // Merge the generated manifests with the downloaded artifacts.
            for entry in std::fs::read_dir(&self.manifest_dir())? {
                let entry = entry?;
                if entry.file_type()?.is_file() {
                    std::fs::rename(entry.path(), self.dl_dir().join(entry.file_name()))?;
                }
            }
        } else {
            // For releases using the legacy build-manifest, we need to clone the rustc monorepo
            // and invoke `./x.py dist hash-and-sign` in it. This won't be needed after 1.48.0 is
            // out in the stable channel.
            self.legacy_update_repo()?;
            self.legacy_configure_rust(&rev)?;
            self.legacy_sign_artifacts()?;
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

    fn legacy_configure_rust(&mut self, rev: &str) -> Result<(), Error> {
        let build = self.build_dir();
        // Only delete the dist artifacts when running the tool locally, to avoid rebuilding
        // bootstrap over and over again.
        if self.config.skip_delete_build_dir {
            let _ = fs::remove_dir_all(build.join("build/dist"));
        } else {
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
            .arg(format!("--release-channel={}", self.config.channel)))?;
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
            self.config.gpg_password_file,
            self.config.upload_addr,
            self.config.upload_dir,
        ));
        std::fs::write(&path, new_config.as_bytes())?;

        Ok(())
    }

    fn current_version_same(&mut self, prev: &str) -> Result<bool, Error> {
        // nightly's always changing
        if self.config.channel == Channel::Nightly {
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
        if self.config.channel != Channel::Nightly {
            return Ok(());
        }

        let mut components = Vec::new();
        for entry in self.dl_dir().read_dir()? {
            let name = entry?.file_name().into_string().unwrap();
            if name.contains("x86_64-unknown-linux-gnu") {
                components.push(name);
            }
        }

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

            let compression_level = flate2::Compression::new(self.config.gzip_compression_level);
            to_recompress
                .par_iter()
                .map(|(xz_path, gz_path)| {
                    println!("recompressing {}...", gz_path.display());

                    let xz = File::open(xz_path)?;
                    let mut xz = XzDecoder::new(xz);
                    let gz = File::create(gz_path)?;
                    let mut gz = flate2::write::GzEncoder::new(gz, compression_level);
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
    fn legacy_sign_artifacts(&mut self) -> Result<(), Error> {
        let build = self.build_dir();
        // This calls `src/tools/build-manifest` from the rustc repo.
        run(Command::new(self.rust_dir().join("x.py"))
            .current_dir(&build)
            .arg("dist")
            .arg("hash-and-sign"))?;

        // Merge all the signatures with the download files, and then sync that
        // whole dir up to the release archives
        for file in self.build_dir().join("build/dist/").read_dir()? {
            let file = file?;
            fs::copy(file.path(), self.dl_dir().join(file.file_name()))?;
        }

        Ok(())
    }

    fn prune_unused_files(&self, shipped_files: &HashSet<PathBuf>) -> Result<(), Error> {
        for entry in std::fs::read_dir(self.dl_dir())? {
            let entry = entry?;
            if let Some(name) = entry.path().file_name() {
                let name = Path::new(name);
                if !shipped_files.contains(name) {
                    std::fs::remove_file(entry.path())?;
                    println!("pruned unused file {}", name.display());
                }
            }
        }

        Ok(())
    }

    fn publish_archive(&mut self) -> Result<(), Error> {
        let bucket = &self.config.upload_bucket;
        let dir = &self.config.upload_dir;
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
        let (version, upload_dir) = match self.config.channel {
            Channel::Stable => {
                let vers = &self.current_version.as_ref().unwrap()[..];
                (vers, "stable")
            }
            Channel::Beta => ("beta", "beta"),
            Channel::Nightly => ("nightly", "nightly"),
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
        let bucket = &self.config.upload_bucket;
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
            &self.config.cloudfront_doc_id,
            &[if dir == "stable" {
                "/*".into()
            } else {
                format!("/{}/*", dir)
            }],
        )
    }

    fn publish_release(&mut self) -> Result<(), Error> {
        let bucket = &self.config.upload_bucket;
        let dir = &self.config.upload_dir;
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
        self.invalidate_cloudfront(&self.config.cloudfront_static_id, &["/dist/*".into()])
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

    fn manifest_dir(&self) -> PathBuf {
        self.work.join("manifests")
    }

    fn build_dir(&self) -> PathBuf {
        self.work.join("build")
    }

    fn s3_artifacts_url(&self, path: &str) -> String {
        format!(
            "s3://{}/{}/{}",
            self.config.download_bucket, self.config.download_dir, path,
        )
    }

    fn aws_s3(&self) -> Command {
        let mut cmd = Command::new("aws");

        // Allow using non-S3 backends with the AWS CLI.
        if let Some(url) = &self.config.s3_endpoint_url {
            cmd.arg("--endpoint-url");
            cmd.arg(url);
        }

        cmd.arg("s3");
        cmd
    }

    fn download_top_level_manifest(&mut self) -> Result<toml::Value, Error> {
        let url = format!(
            "{}/{}/channel-rust-{}.toml",
            self.config.upload_addr, self.config.upload_dir, self.config.channel
        );
        println!("downloading manifest from: {}", url);

        Ok(self
            .download_file(&url)?
            .expect("manifest not found")
            .parse()?)
    }

    fn dated_manifest_exists(&mut self) -> Result<bool, Error> {
        let url = format!(
            "{}/{}/{}/channel-rust-{}.toml",
            self.config.upload_addr, self.config.upload_dir, self.date, self.config.channel,
        );
        println!("checking if manifest exists: {}", url);

        Ok(self.download_file(&url)?.is_some())
    }

    fn download_file(&mut self, url: &str) -> Result<Option<String>, Error> {
        self.handle.reset();
        self.handle.get(true)?;
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
        match self.handle.response_code()? {
            200 => Ok(Some(String::from_utf8(result)?)),
            404 => Ok(None),
            other => anyhow::bail!("unexpected status code while fetching {}: {}", url, other),
        }
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

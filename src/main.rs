#![allow(clippy::rc_buffer)]

mod build_manifest;
mod config;
mod curl_helper;
mod discourse;
mod github;
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
use github::{CreateTag, Github};
use rayon::prelude::*;
use xz2::read::XzDecoder;

use crate::config::{Channel, Config};

const TARGET: &str = env!("TARGET");

const BLOG_PRIMARY_BRANCH: &str = "master";

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
        if self.current_version_same(previous_version)? && !self.config.bypass_startup_checks {
            println!("version hasn't changed, skipping");
            println!("set PROMOTE_RELEASE_BYPASS_STARTUP_CHECKS=1 to bypass the check");
            return Ok(());
        }

        self.assert_all_components_present()?;

        // Ok we've now determined that a release needs to be done.

        let mut signer = Signer::new(&self.config)?;

        let build_manifest = BuildManifest::new(self)?;
        let smoke_test = SmokeTester::new(&[self.smoke_manifest_dir(), self.dl_dir()])?;

        // First of all, the real manifests are generated, pointing to the public download
        // endpoint. This will also collect the list of files shipped in the release (used
        // later to prune the files we're not shipping) and a cache of all the checksums
        // generated by build-manifest.
        let execution = build_manifest.run(
            &format!("{}/{}", self.config.upload_addr, self.config.upload_dir),
            &self.real_manifest_dir(),
        )?;

        // Then another set of manifests is generated pointing to the smoke test server. These
        // manifests will be discarded later.
        build_manifest.run(
            &format!("http://{}/dist", smoke_test.server_addr()),
            &self.smoke_manifest_dir(),
        )?;

        // Removes files that we are not shipping from the files we're about to upload.
        if let Some(shipped_files) = &execution.shipped_files {
            self.prune_unused_files(shipped_files)?;
        }

        // Sign both the downloaded artifacts and all the generated manifests. The signatures
        // of the downloaded files and the real manifests are permanent, while the signatures
        // for the smoke test manifests will be discarded later.
        signer.override_checksum_cache(execution.checksum_cache);
        signer.sign_directory(&self.dl_dir())?;
        signer.sign_directory(&self.real_manifest_dir())?;
        signer.sign_directory(&self.smoke_manifest_dir())?;

        // Ensure the release is downloadable from rustup and can execute a basic binary.
        smoke_test.test(&self.config.channel)?;

        // Merge the generated manifests with the downloaded artifacts.
        for entry in std::fs::read_dir(&self.real_manifest_dir())? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                std::fs::rename(entry.path(), self.dl_dir().join(entry.file_name()))?;
            }
        }

        self.publish_archive()?;
        self.publish_docs()?;
        self.publish_release()?;

        self.invalidate_releases()?;

        // Clean up after ourselves to avoid leaving gigabytes of artifacts
        // around.
        let _ = fs::remove_dir_all(&self.dl_dir());

        // This opens a PR and starts an internals thread announcing a
        // stable dev-release (we distinguish dev by the presence of metadata
        // which lets us know where to create and what to put in the blog).
        self.open_blog()?;

        // We do this last, since it triggers triagebot posting the GitHub
        // release announcement (and since this is not actually really
        // important).
        self.tag_release(&rev, &mut signer)?;

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
                    if self.config.wip_recompress || !gz_path.is_file() {
                        to_recompress.push((path.to_path_buf(), gz_path));
                    }
                }
                Some("gz") if self.config.wip_recompress => {
                    fs::remove_file(&path)?;
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
            .arg("--storage-class")
            .arg(&self.config.storage_class)
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
            .arg("--storage-class")
            .arg(&self.config.storage_class)
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
                .arg("--storage-class")
                .arg(&self.config.storage_class)
                .arg("--delete")
                .arg("--only-show-errors")
                .arg(format!("{}/", docs.display()))
                .arg(&dst))?;
            self.invalidate_docs(version)?;
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
            .arg("--storage-class")
            .arg(&self.config.storage_class)
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

    fn tag_release(&mut self, rustc_commit: &str, signer: &mut Signer) -> Result<(), Error> {
        if self.config.channel != Channel::Stable {
            // We don't tag non-stable releases
            return Ok(());
        }

        let mut github = if let Some(github) = self.config.github() {
            github
        } else {
            eprintln!("Skipping tagging - GitHub credentials not configured");
            return Ok(());
        };

        if let Some(repo) = self.config.rustc_tag_repository.clone() {
            self.tag_repository(signer, &mut github, &repo, rustc_commit)?;

            // Once we've tagged rustc, kick off a thanks workflow run.
            github
                .token("rust-lang/thanks")?
                .workflow_dispatch("ci.yml", "master")?;
        }

        Ok(())
    }

    fn tag_repository(
        &mut self,
        signer: &mut Signer,
        github: &mut Github,
        repository: &str,
        commit: &str,
    ) -> Result<(), Error> {
        let version = self.current_version.as_ref().expect("has current version");
        let tag_name = version.to_owned();
        let username = "rust-lang/promote-release";
        let email = "release-team@rust-lang.org";
        let message = signer.git_signed_tag(
            commit,
            &tag_name,
            username,
            email,
            &format!("{} release", version),
        )?;

        github.token(repository)?.tag(CreateTag {
            commit,
            tag_name: &tag_name,
            message: &message,
            tagger_name: username,
            tagger_email: email,
        })?;

        Ok(())
    }

    fn open_blog(&mut self) -> Result<(), Error> {
        // We rely on the blog variables not being set in production to disable
        // blogging on the actual release date.
        if self.config.channel != Channel::Stable {
            eprintln!("Skipping blogging -- not on stable");
            return Ok(());
        }

        let mut github = if let Some(github) = self.config.github() {
            github
        } else {
            eprintln!("Skipping blogging - GitHub credentials not configured");
            return Ok(());
        };
        let mut discourse = if let Some(discourse) = self.config.discourse() {
            discourse
        } else {
            eprintln!("Skipping blogging - Discourse credentials not configured");
            return Ok(());
        };
        let repository_for_blog = if let Some(repo) = &self.config.blog_repository {
            repo.as_str()
        } else {
            eprintln!("Skipping blogging - blog repository not configured");
            return Ok(());
        };

        let version = self.current_version.as_ref().expect("has current version");
        let internals_contents =
            if let Some(contents) = self.config.blog_contents(version, &self.date, false, None) {
                contents
            } else {
                eprintln!("Skipping internals - insufficient information to create blog post");
                return Ok(());
            };

        let announcements_category = 18;
        let internals_url = discourse.create_topic(
            announcements_category,
            &format!("Rust {} pre-release testing", version),
            &internals_contents,
        )?;
        let blog_contents = if let Some(contents) =
            self.config
                .blog_contents(version, &self.date, true, Some(&internals_url))
        {
            contents
        } else {
            eprintln!("Skipping blogging - insufficient information to create blog post");
            return Ok(());
        };

        // Create a new branch so that we don't need to worry about the file
        // already existing. In practice this *could* collide, but after merging
        // a PR branches should get deleted, so it's very unlikely.
        let name = format!("automation-{:x}", rand::random::<u32>());
        let mut token = github.token(repository_for_blog)?;
        let master_sha = token.get_ref(&format!("heads/{BLOG_PRIMARY_BRANCH}"))?;
        token.create_ref(&format!("refs/heads/{name}"), &master_sha)?;
        token.create_file(
            &name,
            &format!(
                "posts/inside-rust/{}-{}-prerelease.md",
                chrono::Utc::today().format("%Y-%m-%d"),
                version,
            ),
            &blog_contents,
        )?;
        token.create_pr(BLOG_PRIMARY_BRANCH, &name, "Pre-release announcement", "")?;

        Ok(())
    }

    fn dl_dir(&self) -> PathBuf {
        self.work.join("dl")
    }

    fn real_manifest_dir(&self) -> PathBuf {
        self.work.join("manifests")
    }

    fn smoke_manifest_dir(&self) -> PathBuf {
        self.work.join("manifests-smoke")
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
        self.handle.url(url)?;
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

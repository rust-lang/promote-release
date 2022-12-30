use crate::Context;
use anyhow::{Context as _, Error};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
    process::Command,
};
use tar::Archive;
use tempfile::{NamedTempFile, TempDir};
use xz2::read::XzDecoder;

pub(crate) struct BuildManifest<'a> {
    builder: &'a Context,
    executable: NamedTempFile,

    _metadata_dir: TempDir,
    checksum_cache_path: PathBuf,
    shipped_files_path: PathBuf,
}

impl<'a> BuildManifest<'a> {
    pub(crate) fn new(builder: &'a Context) -> Result<Self, Error> {
        let metadata_dir = TempDir::new()?;
        let checksum_cache_path = metadata_dir.path().join("checksum-cache.json");
        let shipped_files_path = metadata_dir.path().join("shipped-files.txt");

        Ok(Self {
            builder,
            executable: Self::extract(builder)
                .context("failed to extract build-manifest from the tarball")?,

            _metadata_dir: metadata_dir,
            checksum_cache_path,
            shipped_files_path,
        })
    }

    pub(crate) fn run(&self, upload_base: &str, dest: &Path) -> Result<Execution, Error> {
        let config = &self.builder.config;

        // Ensure the manifest dir exists but is empty.
        if dest.is_dir() {
            std::fs::remove_dir_all(dest)?;
        }
        std::fs::create_dir_all(dest)?;

        // Ensure the shipped files path does not exists
        if self.shipped_files_path.is_file() {
            std::fs::remove_file(&self.shipped_files_path)?;
        }

        println!("running build-manifest...");
        // build-manifest <input-dir> <output-dir> <date> <upload-addr> <channel>
        let num_threads = self.builder.config.num_threads.to_string();
        let status = Command::new(self.executable.path())
            .arg(self.builder.dl_dir())
            .arg(dest)
            .arg(&self.builder.date)
            .arg(upload_base)
            .arg(config.channel.to_string())
            .env("BUILD_MANIFEST_CHECKSUM_CACHE", &self.checksum_cache_path)
            .env("BUILD_MANIFEST_NUM_THREADS", num_threads)
            .env(
                "BUILD_MANIFEST_SHIPPED_FILES_PATH",
                &self.shipped_files_path,
            )
            .status()
            .context("failed to execute build-manifest")?;

        if status.success() {
            Execution::new(&self.shipped_files_path, &self.checksum_cache_path)
        } else {
            anyhow::bail!("build-manifest failed with status {:?}", status);
        }
    }

    fn extract(builder: &'a Context) -> Result<NamedTempFile, Error> {
        let release = builder.config.channel.release_name(builder);
        let tarball_name = format!("build-manifest-{}-{}", release, crate::TARGET);
        let tarball_path = builder.dl_dir().join(format!("{}.tar.xz", tarball_name));

        let binary_path = Path::new(&tarball_name)
            .join("build-manifest")
            .join("bin")
            .join("build-manifest");

        let tarball_file = BufReader::new(File::open(tarball_path)?);
        let mut tarball = Archive::new(XzDecoder::new(tarball_file));

        let bin = NamedTempFile::new()?;
        tarball
            .entries()?
            .filter_map(|e| e.ok())
            .find(|e| e.path().ok().as_deref() == Some(&binary_path))
            .ok_or_else(|| anyhow::anyhow!("missing build-manifest binary inside the tarball"))?
            .unpack(bin.path())?;

        Ok(bin)
    }
}

pub(crate) struct Execution {
    pub(crate) shipped_files: Option<HashSet<PathBuf>>,
    pub(crate) checksum_cache: HashMap<PathBuf, String>,
}

impl Execution {
    fn new(shipped_files_path: &Path, checksum_cache_path: &Path) -> Result<Self, Error> {
        // Once https://github.com/rust-lang/rust/pull/78196 reaches stable we can assume the
        // "shipped files" file is always generated, and we can remove the Option<_>.
        let shipped_files = if shipped_files_path.is_file() {
            Some(
                std::fs::read_to_string(shipped_files_path)?
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(PathBuf::from)
                    .collect(),
            )
        } else {
            None
        };

        // Once https://github.com/rust-lang/rust/pull/78409 reaches stable we can assume the
        // checksum cache will always be generated, and we can remove the if branch.
        let checksum_cache = if checksum_cache_path.is_file() {
            serde_json::from_slice(&std::fs::read(checksum_cache_path)?)?
        } else {
            HashMap::new()
        };

        Ok(Execution {
            shipped_files,
            checksum_cache,
        })
    }
}

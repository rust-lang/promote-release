use crate::Context;
use anyhow::{Context as _, Error};
use std::{
    collections::HashSet,
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
    tarball_name: String,
    tarball_path: PathBuf,
}

impl<'a> BuildManifest<'a> {
    pub(crate) fn new(builder: &'a Context) -> Self {
        // Precalculate paths used later.
        let release = builder.config.channel.release_name(builder);
        let tarball_name = format!("build-manifest-{}-{}", release, crate::TARGET);
        let tarball_path = builder.dl_dir().join(format!("{}.tar.xz", tarball_name));

        Self {
            builder,
            tarball_name,
            tarball_path,
        }
    }

    pub(crate) fn exists(&self) -> bool {
        self.tarball_path.is_file()
    }

    pub(crate) fn run(&self) -> Result<Execution, Error> {
        let config = &self.builder.config;
        let bin = self
            .extract()
            .context("failed to extract build-manifest from the tarball")?;

        let metadata_dir = TempDir::new()?;
        let shipped_files_path = metadata_dir.path().join("shipped-files.txt");

        println!("running build-manifest...");
        let upload_addr = format!("{}/{}", config.upload_addr, config.upload_dir);
        // build-manifest <input-dir> <output-dir> <date> <upload-addr> <channel>
        let status = Command::new(bin.path())
            .arg(self.builder.dl_dir())
            .arg(self.builder.dl_dir())
            .arg(&self.builder.date)
            .arg(upload_addr)
            .arg(config.channel.to_string())
            .env("BUILD_MANIFEST_SHIPPED_FILES_PATH", &shipped_files_path)
            .status()
            .context("failed to execute build-manifest")?;

        if status.success() {
            Execution::new(&shipped_files_path)
        } else {
            anyhow::bail!("build-manifest failed with status {:?}", status);
        }
    }

    fn extract(&self) -> Result<NamedTempFile, Error> {
        let binary_path = Path::new(&self.tarball_name)
            .join("build-manifest")
            .join("bin")
            .join("build-manifest");

        let tarball_file = BufReader::new(File::open(&self.tarball_path)?);
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
}

impl Execution {
    fn new(shipped_files_path: &Path) -> Result<Self, Error> {
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

        Ok(Execution { shipped_files })
    }
}

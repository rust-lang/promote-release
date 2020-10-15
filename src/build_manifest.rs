use crate::{config::Channel, Context};
use anyhow::{Context as _, Error};
use std::{
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
    process::Command,
};
use tar::Archive;
use tempfile::NamedTempFile;
use xz2::read::XzDecoder;

pub(crate) struct BuildManifest<'a> {
    builder: &'a Context,
    tarball_name: String,
    tarball_path: PathBuf,
}

impl<'a> BuildManifest<'a> {
    pub(crate) fn new(builder: &'a Context) -> Self {
        // Precalculate paths used later.
        let release = match builder.config.channel {
            Channel::Stable => builder.current_version.clone().unwrap(),
            channel => channel.to_string(),
        };
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

    pub(crate) fn run(&self) -> Result<(), Error> {
        let config = &self.builder.config;
        let bin = self
            .extract()
            .context("failed to extract build-manifest from the tarball")?;

        println!("running build-manifest...");
        let upload_addr = format!("{}/{}", config.upload_addr, config.upload_dir);
        // build-manifest <input-dir> <output-dir> <date> <upload-addr> <channel>
        let status = Command::new(bin.path())
            .arg(self.builder.dl_dir())
            .arg(self.builder.dl_dir())
            .arg(&self.builder.date)
            .arg(upload_addr)
            .arg(config.channel.to_string())
            .status()
            .context("failed to execute build-manifest")?;

        if status.success() {
            Ok(())
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

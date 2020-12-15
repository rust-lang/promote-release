use anyhow::Error;
use chrono::Utc;
use pgp::{
    armor::BlockType,
    crypto::HashAlgorithm,
    packet::{Packet, SignatureConfig, SignatureType, SignatureVersion, Subpacket},
    types::{KeyTrait, SecretKeyTrait},
    Deserializable, SignedSecretKey,
};
use rayon::prelude::*;
use sha2::Digest;
use std::{
    collections::HashMap,
    fs::File,
    path::{Path, PathBuf},
    time::Instant,
};

use crate::config::Config;

pub(crate) struct Signer {
    gpg_key: SignedSecretKey,
    gpg_password: String,
    sha256_checksum_cache: HashMap<PathBuf, String>,
}

impl Signer {
    pub(crate) fn new(config: &Config) -> Result<Self, Error> {
        let mut key_file = File::open(&config.gpg_key_file)?;
        let gpg_password = std::fs::read_to_string(&config.gpg_password_file)?;
        Ok(Signer {
            gpg_key: SignedSecretKey::from_armor_single(&mut key_file)?.0,
            gpg_password,
            sha256_checksum_cache: HashMap::new(),
        })
    }

    pub(crate) fn override_checksum_cache(&mut self, new: HashMap<PathBuf, String>) {
        self.sha256_checksum_cache = new;
    }

    pub(crate) fn sign_directory(&self, path: &Path) -> Result<(), Error> {
        let mut paths = Vec::new();
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();

            if !entry.metadata()?.is_file() || should_exclude_path(&path) {
                continue;
            }
            paths.push(path);
        }

        self.sign_batch(&paths)
    }

    fn sign_batch(&self, paths: &[PathBuf]) -> Result<(), Error> {
        let start = Instant::now();
        println!(
            "hashing and signing {} files across {} threads",
            paths.len(),
            rayon::current_num_threads().min(paths.len())
        );

        paths
            .par_iter()
            .map(|path| self.sign(path))
            .collect::<Result<Vec<()>, Error>>()?;

        println!(
            "finished hashing and signing {} files in {:.2?}",
            paths.len(),
            start.elapsed()
        );

        Ok(())
    }

    fn sign(&self, path: &Path) -> Result<(), Error> {
        let data = std::fs::read(path)?;

        // This is creating a hash of the file two times, one in generate_sha256 and one in
        // gpg_sign. Unfortunately it seems like generating a gpg signature of an existing hash is
        // not trivial, and I don't have the time to dig into the RFC to figure out a way to do so.
        //
        // Eventually we should stop generating signatures for each file, and instead create a
        // SHA256SUMS file with the hashes of all the files we're shipping, and sign that.
        self.generate_sha256(path, &data)?;
        self.gpg_sign(path, &data)?;

        Ok(())
    }

    fn generate_sha256(&self, path: &Path, data: &[u8]) -> Result<(), Error> {
        let canonical_path = std::fs::canonicalize(path)?;

        let sha256 = if let Some(cached) = self.sha256_checksum_cache.get(&canonical_path) {
            cached.clone()
        } else {
            let mut digest = sha2::Sha256::default();
            digest.update(data);
            hex::encode(digest.finalize())
        };

        let file_name = path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("missing file name from path"))?
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("the file name is not UTF-8"))?;

        std::fs::write(
            add_suffix(path, ".sha256"),
            format!("{}  {}\n", sha256, file_name),
        )?;

        Ok(())
    }

    fn gpg_sign(&self, path: &Path, data: &[u8]) -> Result<(), Error> {
        let key_function = || self.gpg_password.trim().to_string();
        let now = Utc::now();

        let pubkey = self.gpg_key.public_key();
        let sign_config = SignatureConfig {
            version: SignatureVersion::V4,
            typ: SignatureType::Binary,
            pub_alg: self.gpg_key.algorithm(),
            hash_alg: HashAlgorithm::SHA2_512,
            issuer: Some(pubkey.key_id()),
            created: Some(now),
            hashed_subpackets: vec![
                Subpacket::SignatureCreationTime(now),
                Subpacket::Issuer(pubkey.key_id()),
            ],
            unhashed_subpackets: Vec::new(),
        };

        let mut dest = File::create(add_suffix(path, ".asc"))?;

        let content = Packet::from(sign_config.sign(&self.gpg_key, key_function, data)?);
        pgp::armor::write(&content, BlockType::Signature, &mut dest, None)?;

        Ok(())
    }
}

fn should_exclude_path(path: &Path) -> bool {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("asc") => true,    // GPG signatures
        Some("sha256") => true, // SHA256 checksums
        _ => false,
    }
}

fn add_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut file_name = path.file_name().expect("missing file name").to_os_string();
    file_name.push(suffix);

    let mut path = path.to_path_buf();
    path.set_file_name(file_name);
    path
}

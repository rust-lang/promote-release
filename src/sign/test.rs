use pgp::composed::{KeyType, SecretKeyParamsBuilder};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tempfile::NamedTempFile;

use super::Signer;

fn test_signer(parent_dir: &Path) -> (Signer, NamedTempFile) {
    let mut key_file = NamedTempFile::new_in(parent_dir).unwrap();
    let mut password_file = NamedTempFile::new_in(parent_dir).unwrap();

    let password = "secure password";
    password_file.write_all(password.as_bytes()).unwrap();

    let mut key_params = SecretKeyParamsBuilder::default();
    key_params
        .key_type(KeyType::Rsa(4096))
        .can_sign(true)
        .passphrase(Some(password.to_owned()))
        .primary_user_id("Me <me@example.com>".into());
    let secret_key_params = key_params
        .build()
        .expect("Must be able to create secret key params");

    eprintln!("Generating secret key...");

    let signed_secret_key = secret_key_params
        .generate(&mut rand_pgp::thread_rng())
        .expect("Failed to generate a plain key.");

    eprintln!("Serializing secret key...");

    signed_secret_key
        .to_armored_writer(&mut key_file, Default::default())
        .unwrap();

    let mut pubkey = NamedTempFile::new_in(parent_dir).unwrap();
    signed_secret_key
        .to_public_key()
        .to_armored_writer(&mut pubkey, Default::default())
        .unwrap();

    eprintln!("Wrote fresh secret key to file");

    (
        Signer::new_inner(key_file.path(), password_file.path()).unwrap(),
        pubkey,
    )
}

#[test]
fn artifact() {
    let parent_dir = tempfile::tempdir().unwrap();
    let (signer, pubkey) = test_signer(parent_dir.path());

    let mut channel_file = tempfile::Builder::new()
        .prefix("fake-channel-manifest")
        .tempfile_in(parent_dir.path())
        .unwrap();
    channel_file.write_all(b"hello world").unwrap();
    signer.sign(channel_file.path()).unwrap();

    let gpg_home = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(parent_dir.path())
        .unwrap();

    let mut gpg = std::process::Command::new("gpg");
    let status = gpg
        .env("GNUPGHOME", gpg_home.path())
        .arg("--import")
        .arg(pubkey.path())
        .status()
        .unwrap();
    assert!(status.success());

    let mut gpg = std::process::Command::new("gpg");
    gpg.env("GNUPGHOME", gpg_home.path())
        .arg("--armor")
        .arg("--pinentry-mode")
        .arg("loopback")
        .arg("--verify")
        .arg(channel_file.path().with_added_extension("asc"))
        .arg(&channel_file.path());
    eprintln!("Running {:?}", gpg);
    let status = gpg.status().unwrap();
    assert!(status.success());

    // sha256sum -c "channel-rust-${channel}.toml.sha256"
    let status = dbg!(
        std::process::Command::new("sha256sum")
            .current_dir(parent_dir.path())
            .arg("-c")
            .arg(channel_file.path().with_added_extension("sha256"))
    )
    .status()
    .unwrap();
    assert!(status.success());
}

#[test]
fn git_tag() {
    let parent_dir = tempfile::tempdir().unwrap();
    let (signer, pubkey) = test_signer(parent_dir.path());

    assert!(
        std::process::Command::new("git")
            .current_dir(parent_dir.path())
            .arg("init")
            .status()
            .unwrap()
            .success()
    );

    assert!(
        std::process::Command::new("git")
            .current_dir(parent_dir.path())
            .env("GIT_AUTHOR_NAME", "Me")
            .env("GIT_AUTHOR_EMAIL", "me@example.com")
            .env("GIT_COMMITTER_NAME", "Me")
            .env("GIT_COMMITTER_EMAIL", "me@example.com")
            .arg("commit")
            .arg("--allow-empty")
            .arg("--message")
            .arg("first commit")
            .status()
            .unwrap()
            .success()
    );

    let commit_hash = std::process::Command::new("git")
        .current_dir(parent_dir.path())
        .arg("rev-parse")
        .arg("HEAD")
        .output()
        .unwrap();
    assert!(commit_hash.status.success(), "{:?}", commit_hash);
    let commit_hash = String::from_utf8(commit_hash.stdout).unwrap();
    let commit_hash = commit_hash.trim();

    let tag_name = "test-tag";
    let username = "Me";
    let email = "me@example.com";
    let (message, timestamp) = signer
        .git_signed_tag(
            dbg!(commit_hash),
            &tag_name,
            username,
            email,
            &format!("test-tag #1"),
        )
        .unwrap();

    let mut message_file = tempfile::NamedTempFile::new_in(parent_dir.path()).unwrap();
    message_file.write_all(message.as_bytes()).unwrap();

    assert!(
        std::process::Command::new("git")
            .current_dir(parent_dir.path())
            .env("GIT_AUTHOR_NAME", "Me")
            .env("GIT_AUTHOR_EMAIL", "me@example.com")
            .env("GIT_COMMITTER_NAME", "Me")
            .env("GIT_COMMITTER_EMAIL", "me@example.com")
            // Note: This is critical. `git-tag` internally produces a tag whose payload contains
            // this datel. The signed message *must* match that date exactly (otherwise it's not
            // signing the right payload).
            .env(
                "GIT_COMMITTER_DATE",
                format!("{} +0000", timestamp.timestamp())
            )
            .arg("tag")
            .arg("-a")
            .arg("--cleanup=verbatim")
            .arg("-F")
            .arg(message_file.path())
            .arg(&tag_name)
            .arg(&commit_hash)
            .status()
            .unwrap()
            .success()
    );

    let gpg_home = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(parent_dir.path())
        .unwrap();

    let mut gpg = std::process::Command::new("gpg");
    let status = gpg
        .env("GNUPGHOME", gpg_home.path())
        .arg("--import")
        .arg(pubkey.path())
        .status()
        .unwrap();
    assert!(status.success());

    println!("### git show tag:");
    assert!(
        std::process::Command::new("git")
            .current_dir(parent_dir.path())
            .env("GNUPGHOME", gpg_home.path())
            .arg("show")
            .arg("--format=raw")
            .arg(&tag_name)
            .status()
            .unwrap()
            .success()
    );

    println!("### git verify-tag:");
    assert!(
        std::process::Command::new("git")
            .current_dir(parent_dir.path())
            .env("GNUPGHOME", gpg_home.path())
            .arg("verify-tag")
            .arg("--verbose")
            .arg(&tag_name)
            .status()
            .unwrap()
            .success()
    );
}

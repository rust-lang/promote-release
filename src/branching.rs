use crate::Context;

impl Context {
    /// Let $stable, $beta, $master be the tips of each branch (when starting).
    ///
    /// * Set stable to $beta.
    /// * Set beta to $master (ish, look for the version bump).
    /// * Create a rust-lang/cargo branch for the appropriate beta commit.
    /// * Post a PR against the newly created beta branch bump src/ci/channel to `beta`.
    pub fn do_branching(&mut self) -> anyhow::Result<()> {
        let mut github = if let Some(github) = self.config.github() {
            github
        } else {
            eprintln!("Skipping branching -- github credentials not configured");
            return Ok(());
        };
        let mut token = github.token("rust-lang/rust")?;
        let bump_commit = token.last_commit_for_file("src/version")?;
        let prebump_sha = bump_commit.parents[0].sha.clone();
        let beta_sha = token.get_ref("heads/beta")?;

        let stable_version = token.read_file(Some("stable"), "src/version")?;
        let beta_version = token.read_file(Some("beta"), "src/version")?;
        let future_beta_version = token.read_file(Some(&prebump_sha), "src/version")?;

        // Check that we've not already promoted. Rather than trying to assert
        // +1 version numbers, we instead have a simpler check that all the
        // versions are unique -- before promotion we should have:
        //
        // * stable @ 1.61.0
        // * beta @ 1.62.0
        // * prebump @ 1.63.0
        //
        // and after promotion we will have (if we were to read the files again):
        //
        // * stable @ 1.62.0
        // * beta @ 1.63.0
        // * prebump @ 1.63.0
        //
        // In this state, if we try to promote again, we want to bail out. The
        // stable == beta check isn't as useful, but still nice to have.
        if stable_version.content()? == beta_version.content()? {
            anyhow::bail!(
                "Stable and beta have the same version: {}; refusing to promote branches.",
                stable_version.content()?.trim()
            );
        }
        if beta_version.content()? == future_beta_version.content()? {
            anyhow::bail!(
                "Beta and pre-bump master ({}) have the same version: {}; refusing to promote branches.",
                prebump_sha,
                beta_version.content()?.trim()
            );
        }

        // No need to disable branch protection, as the promote-release app is
        // specifically authorized to force-push to these branches.
        token.update_ref("heads/stable", &beta_sha, true)?;
        token.update_ref("heads/beta", &prebump_sha, true)?;

        let cargo_sha = token
            .read_file(Some(&prebump_sha), "src/tools/cargo")?
            .submodule_sha()
            .to_owned();

        let mut github = self.config.github().unwrap();
        let mut token = github.token("rust-lang/cargo")?;
        let new_beta = future_beta_version.content()?.trim().to_owned();
        token.create_ref(&format!("refs/heads/rust-{}", new_beta), &cargo_sha)?;

        Ok(())
    }
}

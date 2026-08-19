use crate::run;
use anyhow::Context;
use std::process::Command;

pub struct S3Client {
    endpoint_url: Option<String>,
}

impl S3Client {
    pub fn new(endpoint_url: Option<String>) -> Self {
        Self { endpoint_url }
    }

    pub fn cmd(&self) -> Command {
        let mut cmd = Command::new("aws");

        // Allow using non-S3 backends with the AWS CLI.
        if let Some(url) = &self.endpoint_url {
            cmd.arg("--endpoint-url");
            cmd.arg(url);
        }

        cmd.arg("s3");
        cmd
    }

    /// Download data from S3.
    pub fn download(&self, s3_url: &str, local_path: &str) -> anyhow::Result<()> {
        run(self
            .cmd()
            .arg("cp")
            .arg("--recursive")
            .arg("--only-show-errors")
            .arg(s3_url)
            .arg(local_path))
        .context("Cannot download from S3")?;
        Ok(())
    }
}

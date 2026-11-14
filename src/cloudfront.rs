use crate::run;
use std::path::Path;
use std::process::Command;

/// Invalidate a website in AWS CloudFront.
pub fn invalidate_cloudfront(
    dir: &Path,
    distribution_id: &str,
    paths: &[String],
) -> anyhow::Result<()> {
    let json = serde_json::json!({
        "Paths": {
            "Items": paths,
            "Quantity": paths.len(),
        },
        "CallerReference": format!("rct-{}", rand::random::<u64>()),
    })
    .to_string();
    let dst = dir.join("payload.json");
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

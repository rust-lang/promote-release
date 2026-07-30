use anyhow::Error;
use promote_release::{Context, RustcConfig};
use std::env;

// Called as:
//
//  $prog work/dir
fn main() -> Result<(), Error> {
    let mut context = Context::new(
        env::current_dir()?.join(env::args_os().nth(1).unwrap()),
        RustcConfig::from_env()?,
    )?;
    context.run()
}

fn main() {
    // Prevent a rebuild every time a non-Rust file is changed.
    println!("cargo:rerun-if-changed=build.rs");

    // Provide the current target as environment variable.
    println!(
        "cargo:rustc-env=TARGET={}",
        std::env::var("TARGET").unwrap()
    );
}

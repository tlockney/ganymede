//! Build details for `ganymede --version` and `ganymede info`: the target
//! triple, the profile, and the git commit when built from a checkout.

use std::process::Command;

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    let profile = std::env::var("PROFILE").unwrap_or_default();
    println!("cargo:rustc-env=GANYMEDE_BUILD_TARGET={target}");
    println!("cargo:rustc-env=GANYMEDE_BUILD_PROFILE={profile}");
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    // A build outside a checkout (a source tarball) can pass it in.
    let commit = std::env::var("GANYMEDE_BUILD_COMMIT")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| git(&["rev-parse", "--short=12", "HEAD"]))
        .unwrap_or_default();
    println!("cargo:rustc-env=GANYMEDE_BUILD_COMMIT={commit}");
    // Rebuild when HEAD moves, not only when this crate's files change.
    if let Some(head) = git(&["rev-parse", "--path-format=absolute", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={head}");
    }
    if let Some(refs) = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]) {
        println!("cargo:rerun-if-changed={refs}/refs/heads");
        println!("cargo:rerun-if-changed={refs}/packed-refs");
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=GANYMEDE_BUILD_COMMIT");
}

//! Records the tag and commit this build came from, for the startup banner.

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Splits `git describe --long --always --dirty` into the nearest tag, how
/// many commits ago it was, and whether the working tree is dirty.
fn parse(describe: &str) -> (Option<String>, u64, bool) {
    let (rest, dirty) = match describe.strip_suffix("-dirty") {
        Some(rest) => (rest, true),
        None => (describe, false),
    };
    let Some((rest, hash)) = rest.rsplit_once('-') else {
        return (None, 0, dirty);
    };
    if !hash.starts_with('g') {
        return (None, 0, dirty);
    }
    let Some((tag, count)) = rest.rsplit_once('-') else {
        return (None, 0, dirty);
    };
    match count.parse() {
        Ok(count) => (Some(tag.to_string()), count, dirty),
        Err(_) => (None, 0, dirty),
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.parent().unwrap().parent().unwrap();
    println!("cargo:rerun-if-changed=build.rs");
    // Rebuild when the checked-out commit, a tag or the dirty state changes.
    if let Some(git_dir) = git(root, &["rev-parse", "--absolute-git-dir"]) {
        for name in ["HEAD", "index", "packed-refs", "refs"] {
            println!(
                "cargo:rerun-if-changed={}",
                Path::new(&git_dir).join(name).display()
            );
        }
    }

    let (mut tag, mut count, mut dirty) = match git(
        root,
        &["describe", "--tags", "--long", "--always", "--dirty"],
    ) {
        Some(describe) => parse(&describe),
        None => (None, 0, false),
    };
    // Release builds are shallow and may not have the tags; the workflow
    // knows the tag it is building.
    if tag.is_none() && std::env::var("GITHUB_REF_TYPE").as_deref() == Ok("tag") {
        if let Ok(name) = std::env::var("GITHUB_REF_NAME") {
            if name.starts_with('v') {
                tag = Some(name);
                count = 0;
                dirty = false;
            }
        }
    }

    if let Some(tag) = tag {
        println!("cargo:rustc-env=GRASSLINE_GIT_TAG={tag}");
    }
    println!("cargo:rustc-env=GRASSLINE_GIT_COUNT={count}");
    println!("cargo:rustc-env=GRASSLINE_GIT_DIRTY={}", u8::from(dirty));
    if let Some(commit) = git(root, &["rev-parse", "--short=7", "HEAD"]) {
        println!("cargo:rustc-env=GRASSLINE_GIT_COMMIT={commit}");
    }
}

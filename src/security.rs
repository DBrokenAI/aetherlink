//! Path sandbox + emergency bypass switch.
//!
//! `apply_guarded_change` and `check_change` go through this module before
//! ever touching disk. Two jobs:
//!
//! 1. **Sandbox.** Resolve the proposed file path (including `..` segments,
//!    symlinks, etc.) against the project root and refuse to proceed if the
//!    real path falls outside. Without this, a prompt-injected agent could
//!    coax AetherLink into writing `../../../Windows/System32/cmd.exe`.
//! 2. **Bypass.** A file named `.aetherlink_bypass` in the project root acts
//!    as a "break glass" override that lets writes through even when the
//!    validator says no. Used for emergencies when you can't afford for a
//!    bug in the validator to lock you out of your own code.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

/// Filename of the kill switch. Presence in the project root engages bypass.
pub const BYPASS_FILENAME: &str = ".aetherlink_bypass";

/// True if the project has a `.aetherlink_bypass` file in its root.
pub fn bypass_engaged(project_root: &Path) -> bool {
    project_root.join(BYPASS_FILENAME).exists()
}

/// Resolve a target path to its real, canonical form and confirm it lives
/// inside the canonical project root. Returns the canonical target path.
///
/// Handles three tricky cases:
/// - **Existing files** are canonicalized directly via `std::fs::canonicalize`,
///   which resolves symlinks and `..` segments to their real form.
/// - **Brand-new files** can't be canonicalized (the OS errors), so we walk
///   up to the first existing ancestor, canonicalize that, then re-append
///   the missing tail components. This still resolves any symlinks in the
///   ancestor chain.
/// - **Relative target paths** are resolved against the (un-canonicalized)
///   root before canonicalization, so callers don't have to pre-join.
pub fn canonicalize_under_root(project_root: &Path, target: &Path) -> Result<PathBuf> {
    let canon_root = fs::canonicalize(project_root)
        .with_context(|| format!("canonicalizing project root {}", project_root.display()))?;

    // If the caller passed a relative target, resolve it against the project root.
    let absolute_target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        project_root.join(target)
    };

    let canon_target = canonicalize_existing_or_parent(&absolute_target)?;

    if !canon_target.starts_with(&canon_root) {
        return Err(anyhow!(
            "PATH ESCAPE BLOCKED: '{}' resolves to '{}', which is outside the project root '{}'.",
            target.display(),
            canon_target.display(),
            canon_root.display()
        ));
    }

    Ok(canon_target)
}

/// Canonicalize a path, even if its leaf doesn't exist yet.
///
/// Walks up to the first existing ancestor, canonicalizes that, and re-appends
/// the missing components. The walked-down portion can't contain symlinks (it
/// doesn't exist), so this is sound: any symlinks in the chain were already
/// resolved when we canonicalized the existing ancestor.
fn canonicalize_existing_or_parent(target: &Path) -> Result<PathBuf> {
    if target.exists() {
        return fs::canonicalize(target)
            .with_context(|| format!("canonicalizing {}", target.display()));
    }

    let mut probe = target.to_path_buf();
    let mut tail: Vec<OsString> = Vec::new();

    loop {
        // Take the last component off `probe` and remember it as part of the
        // tail to re-append after we find an existing ancestor.
        let Some(name) = probe.file_name().map(|s| s.to_os_string()) else {
            return Err(anyhow!(
                "could not find any existing ancestor of {}",
                target.display()
            ));
        };
        if !probe.pop() {
            return Err(anyhow!(
                "could not find any existing ancestor of {}",
                target.display()
            ));
        }
        tail.push(name);

        if probe.exists() {
            break;
        }
    }

    let mut canon = fs::canonicalize(&probe)
        .with_context(|| format!("canonicalizing existing ancestor {}", probe.display()))?;
    for component in tail.into_iter().rev() {
        canon.push(component);
    }
    Ok(canon)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;

    fn tempdir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("aetherlink-sec-{label}-{nanos}"));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn allows_existing_file_under_root() {
        let root = tempdir("ok-existing");
        let sub = root.join("src");
        fs::create_dir_all(&sub).unwrap();
        let f = sub.join("foo.rs");
        File::create(&f).unwrap().write_all(b"x").unwrap();

        let canon = canonicalize_under_root(&root, &f).unwrap();
        assert!(canon.starts_with(fs::canonicalize(&root).unwrap()));
    }

    #[test]
    fn allows_brand_new_file_in_existing_subdir() {
        let root = tempdir("ok-newfile");
        fs::create_dir_all(root.join("src")).unwrap();
        let target = root.join("src").join("does_not_exist_yet.rs");

        let canon = canonicalize_under_root(&root, &target).unwrap();
        assert!(canon.starts_with(fs::canonicalize(&root).unwrap()));
        assert!(canon.ends_with("does_not_exist_yet.rs"));
    }

    #[test]
    fn allows_brand_new_file_in_brand_new_subdir() {
        let root = tempdir("ok-newdirs");
        // Neither `new_module` nor the file inside it exist yet.
        let target = root.join("src").join("new_module").join("foo.rs");

        let canon = canonicalize_under_root(&root, &target).unwrap();
        assert!(canon.starts_with(fs::canonicalize(&root).unwrap()));
    }

    #[test]
    fn rejects_dotdot_escape_to_sibling_dir() {
        let parent = tempdir("escape-sibling");
        let root = parent.join("project");
        fs::create_dir_all(&root).unwrap();
        let evil_neighbor = parent.join("not_my_project");
        fs::create_dir_all(&evil_neighbor).unwrap();
        let evil_target = evil_neighbor.join("steal_me.txt");
        File::create(&evil_target).unwrap().write_all(b"x").unwrap();

        // Try to reach it via ../not_my_project/steal_me.txt from the project root.
        let attempt = root.join("..").join("not_my_project").join("steal_me.txt");
        let err = canonicalize_under_root(&root, &attempt).unwrap_err();
        assert!(err.to_string().contains("PATH ESCAPE BLOCKED"));
    }

    #[test]
    fn rejects_absolute_path_outside_root() {
        let root = tempdir("escape-absolute");
        let outside = tempdir("outside-target");
        let outside_file = outside.join("evil.txt");
        File::create(&outside_file).unwrap().write_all(b"x").unwrap();

        let err = canonicalize_under_root(&root, &outside_file).unwrap_err();
        assert!(err.to_string().contains("PATH ESCAPE BLOCKED"));
    }

    #[test]
    fn allows_dotdot_that_resolves_back_inside() {
        // src/../src/foo.rs is just src/foo.rs
        let root = tempdir("ok-loopback");
        fs::create_dir_all(root.join("src")).unwrap();
        let target = root.join("src").join("..").join("src").join("foo.rs");

        let canon = canonicalize_under_root(&root, &target).unwrap();
        assert!(canon.starts_with(fs::canonicalize(&root).unwrap()));
    }

    #[test]
    fn relative_target_resolves_against_project_root() {
        let root = tempdir("ok-relative");
        fs::create_dir_all(root.join("src")).unwrap();
        let canon = canonicalize_under_root(&root, Path::new("src/new.rs")).unwrap();
        assert!(canon.starts_with(fs::canonicalize(&root).unwrap()));
        assert!(canon.ends_with("new.rs"));
    }

    #[test]
    fn bypass_engaged_detects_kill_switch_file() {
        let root = tempdir("bypass");
        assert!(!bypass_engaged(&root));
        File::create(root.join(BYPASS_FILENAME)).unwrap();
        assert!(bypass_engaged(&root));
    }
}

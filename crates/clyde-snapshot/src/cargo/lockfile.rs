//! `Cargo.lock` parsing.
//!
//! Reduced to what the fetch policy needs: package name, version, and source.
//! Clyde never resolves dependencies itself — cargo runs with `--locked` so the
//! lockfile is authoritative and resolution cannot drift during a fetch.

use std::path::Path;

use clyde_core::Digest;
use clyde_policy::access::{LockedPackage, LockfileSummary, PackageSource};

use crate::error::{Result, SnapshotError};

/// A parsed lockfile plus the digest of the exact bytes it was parsed from.
///
/// The digest is what a dependency bundle records as "the lockfile I satisfy",
/// so it must come from the same read as the parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedLockfile {
    pub summary: LockfileSummary,
    pub digest: Digest,
}

/// Parses lockfile text.
pub fn parse(path: &Path, text: &str) -> Result<ParsedLockfile> {
    let value: toml::Value = toml::from_str(text).map_err(|error| SnapshotError::Lockfile {
        path: path.to_path_buf(),
        detail: format!("{error}"),
    })?;
    let packages = value
        .get("package")
        .and_then(toml::Value::as_array)
        .map(|items| items.as_slice())
        .unwrap_or(&[]);

    let mut parsed = Vec::new();
    for package in packages {
        let Some(table) = package.as_table() else {
            continue;
        };
        let (Some(name), Some(version)) = (
            table.get("name").and_then(toml::Value::as_str),
            table.get("version").and_then(toml::Value::as_str),
        ) else {
            return Err(SnapshotError::Lockfile {
                path: path.to_path_buf(),
                detail: "a package entry has no name or version".to_owned(),
            });
        };
        parsed.push(LockedPackage {
            name: name.to_owned(),
            version: version.to_owned(),
            source: classify_source(table.get("source").and_then(toml::Value::as_str)),
        });
    }
    parsed.sort();

    Ok(ParsedLockfile {
        summary: LockfileSummary { packages: parsed },
        digest: Digest::of_bytes(text.as_bytes()),
    })
}

/// Reads and parses a lockfile.
pub fn read(path: &Path) -> Result<ParsedLockfile> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| SnapshotError::io(format!("reading {path:?}"), error))?;
    parse(path, &text)
}

/// Classifies a lockfile `source` value.
///
/// A package with no source is a workspace member or a path dependency: it comes
/// from the repository, not from a registry.
fn classify_source(source: Option<&str>) -> PackageSource {
    let Some(source) = source else {
        return PackageSource::Path;
    };
    if let Some(rest) = source.strip_prefix("git+") {
        let (url, rev) = match rest.split_once('#') {
            Some((url, rev)) => (url.to_owned(), Some(rev.to_owned())),
            None => (rest.to_owned(), None),
        };
        // A query string such as `?branch=main` is part of the identity but not
        // of the URL a human would recognise.
        let url = url.split('?').next().unwrap_or(&url).to_owned();
        return PackageSource::Git { url, rev };
    }
    PackageSource::Registry {
        index: source.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]
    use super::*;

    const SAMPLE: &str = r#"
version = 4

[[package]]
name = "serde"
version = "1.0.229"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "abc"

[[package]]
name = "local-crate"
version = "0.1.0"

[[package]]
name = "sketchy"
version = "0.2.0"
source = "git+https://example.test/sketchy?branch=main#deadbeef"
"#;

    #[test]
    fn packages_are_parsed_with_their_sources() {
        let parsed = parse(Path::new("Cargo.lock"), SAMPLE).unwrap();
        assert_eq!(parsed.summary.packages.len(), 3);
        let by_name = |name: &str| {
            parsed
                .summary
                .packages
                .iter()
                .find(|package| package.name == name)
                .unwrap()
                .clone()
        };
        assert!(matches!(
            by_name("serde").source,
            PackageSource::Registry { .. }
        ));
        assert_eq!(by_name("local-crate").source, PackageSource::Path);
        match by_name("sketchy").source {
            PackageSource::Git { url, rev } => {
                assert_eq!(url, "https://example.test/sketchy");
                assert_eq!(rev.as_deref(), Some("deadbeef"));
            }
            other => panic!("expected a git source, got {other:?}"),
        }
    }

    #[test]
    fn the_digest_covers_the_exact_bytes_parsed() {
        let parsed = parse(Path::new("Cargo.lock"), SAMPLE).unwrap();
        assert_eq!(parsed.digest, Digest::of_bytes(SAMPLE.as_bytes()));
        let altered = parse(Path::new("Cargo.lock"), &format!("{SAMPLE}\n")).unwrap();
        assert_ne!(parsed.digest, altered.digest);
    }

    #[test]
    fn parsing_is_order_independent_in_its_result() {
        let reversed = r#"
[[package]]
name = "b"
version = "1.0.0"

[[package]]
name = "a"
version = "1.0.0"
"#;
        let forward = r#"
[[package]]
name = "a"
version = "1.0.0"

[[package]]
name = "b"
version = "1.0.0"
"#;
        assert_eq!(
            parse(Path::new("Cargo.lock"), reversed).unwrap().summary,
            parse(Path::new("Cargo.lock"), forward).unwrap().summary
        );
    }

    #[test]
    fn an_empty_lockfile_parses_to_nothing() {
        let parsed = parse(Path::new("Cargo.lock"), "version = 4\n").unwrap();
        assert!(parsed.summary.packages.is_empty());
    }

    #[test]
    fn malformed_lockfiles_are_refused() {
        assert!(parse(Path::new("Cargo.lock"), "not toml {{{").is_err());
        assert!(
            parse(Path::new("Cargo.lock"), "[[package]]\nname = \"x\"\n").is_err(),
            "a package with no version must be refused, not silently dropped"
        );
    }
}

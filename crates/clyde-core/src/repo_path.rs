//! Workspace-root-relative repository paths.
//!
//! Path validation lives in exactly one place and is exercised by unit tests
//! from Phase 0 (schema reference: Paths). A `RepoPath` in hand is normalised,
//! relative, and free of traversal, so downstream code never re-checks it.

use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};

use crate::error::ValidationError;

/// Upper bound on a repository path, so untrusted input cannot allocate without
/// limit and cannot exceed common filesystem limits after joining.
const MAX_PATH_BYTES: usize = 1024;

/// A validated, normalised, workspace-root-relative path.
///
/// The empty path is not representable; the workspace root itself is written as
/// `"."` and normalises to [`RepoPath::root`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct RepoPath(String);

impl RepoPath {
    /// The workspace root.
    pub fn root() -> Self {
        Self(".".to_owned())
    }

    /// Parses and normalises a workspace-relative path.
    ///
    /// Rejects absolute paths, paths that escape the root, and any component
    /// that is empty or `..` after normalisation. Windows-style prefixes and
    /// root components are rejected rather than silently dropped.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ValidationError> {
        let raw = value.as_ref();
        if raw.is_empty() {
            return Err(ValidationError::EmptyRepoPath);
        }
        if raw.len() > MAX_PATH_BYTES {
            return Err(ValidationError::FieldTooLong {
                field: "repo path",
                max: MAX_PATH_BYTES,
            });
        }
        if raw.contains('\0') {
            return Err(ValidationError::InvalidRepoPathComponent {
                path: raw.to_owned(),
                reason: "contains a NUL byte",
            });
        }
        let path = Path::new(raw);
        if path.is_absolute() {
            return Err(ValidationError::AbsoluteRepoPath {
                path: raw.to_owned(),
            });
        }

        let mut parts: Vec<&str> = Vec::new();
        for component in path.components() {
            match component {
                Component::CurDir => {}
                Component::Normal(part) => match part.to_str() {
                    Some(part) => parts.push(part),
                    None => {
                        return Err(ValidationError::InvalidRepoPathComponent {
                            path: raw.to_owned(),
                            reason: "is not valid UTF-8",
                        });
                    }
                },
                // `..` is rejected outright rather than resolved: resolving it
                // would make `a/../b` and `b` the same path, and a caller that
                // wrote `..` meant something this type cannot express.
                Component::ParentDir => {
                    return Err(ValidationError::RepoPathEscapesRoot {
                        path: raw.to_owned(),
                    });
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(ValidationError::AbsoluteRepoPath {
                        path: raw.to_owned(),
                    });
                }
            }
        }

        if parts.is_empty() {
            return Ok(Self::root());
        }
        Ok(Self(parts.join("/")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_root(&self) -> bool {
        self.0 == "."
    }

    /// The path components, empty for the root.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
    }

    /// Whether `self` is `other` or lies inside the subtree rooted at `other`.
    ///
    /// Comparison is component-wise, so `src/apple` is not inside `src/app`.
    pub fn is_within(&self, other: &RepoPath) -> bool {
        if other.is_root() {
            return true;
        }
        let mut mine = self.components();
        for part in other.components() {
            match mine.next() {
                Some(candidate) if candidate == part => {}
                _ => return false,
            }
        }
        true
    }

    /// Joins a relative segment, validating the result.
    pub fn join(&self, segment: impl AsRef<str>) -> Result<Self, ValidationError> {
        let segment = segment.as_ref();
        if self.is_root() {
            return Self::parse(segment);
        }
        Self::parse(format!("{}/{segment}", self.0))
    }

    /// The parent path, or `None` for the root.
    pub fn parent(&self) -> Option<Self> {
        if self.is_root() {
            return None;
        }
        match self.0.rsplit_once('/') {
            Some((head, _)) => Some(Self(head.to_owned())),
            None => Some(Self::root()),
        }
    }

    /// The final component, or `None` for the root.
    pub fn file_name(&self) -> Option<&str> {
        if self.is_root() {
            return None;
        }
        self.0.rsplit('/').next()
    }

    /// Resolves against an absolute workspace root.
    ///
    /// The result is lexical: it does not follow symlinks. Callers that hand the
    /// path to the kernel must additionally confirm the resolved path stays
    /// inside the root, which [`resolve_within`] does.
    pub fn to_host_path(&self, root: &Path) -> PathBuf {
        if self.is_root() {
            return root.to_path_buf();
        }
        root.join(&self.0)
    }

    /// Resolves against an absolute workspace root and confirms, by canonical
    /// path, that the result is still inside it.
    ///
    /// This is the symlink check: a `RepoPath` cannot contain `..`, but a
    /// component may *be* a symlink pointing out of the workspace, and only the
    /// filesystem can say so.
    pub fn resolve_within(&self, root: &Path) -> Result<PathBuf, ValidationError> {
        let candidate = self.to_host_path(root);
        let canonical_root =
            root.canonicalize()
                .map_err(|_| ValidationError::RepoPathEscapesRoot {
                    path: self.0.clone(),
                })?;
        match candidate.canonicalize() {
            Ok(resolved) => {
                if resolved.starts_with(&canonical_root) {
                    Ok(resolved)
                } else {
                    Err(ValidationError::RepoPathEscapesRoot {
                        path: self.0.clone(),
                    })
                }
            }
            // A path that does not exist yet cannot be traversing a symlink out
            // of the workspace *at its final component*, but its parents can be,
            // so the deepest existing ancestor is checked instead.
            Err(_) => {
                let mut ancestor = candidate.as_path();
                loop {
                    match ancestor.parent() {
                        Some(parent) => {
                            if let Ok(resolved) = parent.canonicalize() {
                                return if resolved.starts_with(&canonical_root) {
                                    Ok(candidate)
                                } else {
                                    Err(ValidationError::RepoPathEscapesRoot {
                                        path: self.0.clone(),
                                    })
                                };
                            }
                            ancestor = parent;
                        }
                        None => {
                            return Err(ValidationError::RepoPathEscapesRoot {
                                path: self.0.clone(),
                            });
                        }
                    }
                }
            }
        }
    }
}

impl fmt::Display for RepoPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RepoPath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

/// Whether `path` lies within any of `roots`.
pub fn is_within_any<'a>(path: &RepoPath, roots: impl IntoIterator<Item = &'a RepoPath>) -> bool {
    roots.into_iter().any(|root| path.is_within(root))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn normalises_redundant_components() {
        assert_eq!(
            RepoPath::parse("./crates/./core").unwrap().as_str(),
            "crates/core"
        );
        assert_eq!(
            RepoPath::parse("crates//core").unwrap().as_str(),
            "crates/core"
        );
        assert_eq!(RepoPath::parse(".").unwrap(), RepoPath::root());
        assert_eq!(RepoPath::parse("./").unwrap(), RepoPath::root());
    }

    #[test]
    fn rejects_traversal_and_absolute() {
        assert!(matches!(
            RepoPath::parse("../etc/passwd"),
            Err(ValidationError::RepoPathEscapesRoot { .. })
        ));
        // Rejected even though it would normalise back inside the root: a
        // caller writing `..` meant something this type does not express.
        assert!(matches!(
            RepoPath::parse("crates/../crates"),
            Err(ValidationError::RepoPathEscapesRoot { .. })
        ));
        assert!(matches!(
            RepoPath::parse("/etc/passwd"),
            Err(ValidationError::AbsoluteRepoPath { .. })
        ));
    }

    #[test]
    fn rejects_empty_and_nul() {
        assert_eq!(RepoPath::parse(""), Err(ValidationError::EmptyRepoPath));
        assert!(RepoPath::parse("a\0b").is_err());
    }

    #[test]
    fn subtree_containment_is_component_wise() {
        let app = RepoPath::parse("src/app").unwrap();
        assert!(RepoPath::parse("src/app").unwrap().is_within(&app));
        assert!(RepoPath::parse("src/app/main.rs").unwrap().is_within(&app));
        // The prefix trap: `src/apple` must not be inside `src/app`.
        assert!(!RepoPath::parse("src/apple").unwrap().is_within(&app));
        assert!(!RepoPath::parse("src").unwrap().is_within(&app));
        assert!(
            RepoPath::parse("anything/at/all")
                .unwrap()
                .is_within(&RepoPath::root())
        );
    }

    #[test]
    fn parent_and_file_name() {
        let p = RepoPath::parse("a/b/c.rs").unwrap();
        assert_eq!(p.file_name(), Some("c.rs"));
        assert_eq!(p.parent().unwrap().as_str(), "a/b");
        assert_eq!(
            RepoPath::parse("a").unwrap().parent().unwrap(),
            RepoPath::root()
        );
        assert_eq!(RepoPath::root().parent(), None);
        assert_eq!(RepoPath::root().file_name(), None);
    }

    #[test]
    fn join_validates_the_result() {
        let base = RepoPath::parse("crates").unwrap();
        assert_eq!(base.join("core/src").unwrap().as_str(), "crates/core/src");
        assert!(base.join("../escape").is_err());
        assert_eq!(RepoPath::root().join("a").unwrap().as_str(), "a");
    }

    #[test]
    fn ordering_is_stable_for_set_membership() {
        let mut set = BTreeSet::new();
        set.insert(RepoPath::parse("b").unwrap());
        set.insert(RepoPath::parse("a").unwrap());
        set.insert(RepoPath::parse("./a").unwrap());
        assert_eq!(set.len(), 2, "normalised paths must deduplicate");
    }

    #[test]
    fn resolve_within_rejects_symlink_escape() {
        let tmp = std::env::temp_dir().join(format!("clyde-repopath-{}", std::process::id()));
        let root = tmp.join("root");
        let outside = tmp.join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret"), b"x").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();

        let inside = RepoPath::parse("link/secret").unwrap();
        assert!(matches!(
            inside.resolve_within(&root),
            Err(ValidationError::RepoPathEscapesRoot { .. })
        ));

        std::fs::write(root.join("real"), b"y").unwrap();
        assert!(
            RepoPath::parse("real")
                .unwrap()
                .resolve_within(&root)
                .is_ok()
        );
        // A path that does not exist yet resolves, so callers can create files.
        assert!(
            RepoPath::parse("new.rs")
                .unwrap()
                .resolve_within(&root)
                .is_ok()
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn is_within_any_matches_any_root() {
        let roots = [
            RepoPath::parse("src").unwrap(),
            RepoPath::parse("docs").unwrap(),
        ];
        let target = RepoPath::parse("docs/readme.md").unwrap();
        assert!(is_within_any(&target, roots.iter()));
        let other = RepoPath::parse("tests/x").unwrap();
        assert!(!is_within_any(&other, roots.iter()));
    }
}

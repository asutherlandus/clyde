//! Build-closure computation.
//!
//! Cargo cannot build a subtree in isolation. The closure for a task on
//! `backend/auth` is the workspace root manifest, `Cargo.lock`, cargo and
//! toolchain configuration, **every** workspace member's manifest (manifests
//! only, not their sources), and full source for the target crate and its
//! transitive in-repo path dependencies.
//!
//! This routinely exceeds the lease's edit scope, and that is expected:
//! read-only input is a confidentiality delta, not an authority one, since the
//! write set remains the lease's edit paths.
//!
//! Getting this wrong is the most likely cause of "Clyde can't build my
//! project", which is why it is fixture-tested across the four layouts that
//! actually differ.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use clyde_core::repo_path::RepoPath;

use crate::error::{Result, SnapshotError};

/// The computed closure.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildClosure {
    /// The workspace root manifest, which `version.workspace`,
    /// `dependencies.*.workspace`, and `[lints] workspace` resolve against.
    pub root_manifest: Option<RepoPath>,
    /// `Cargo.lock`, cargo configuration, and the toolchain file, which cargo
    /// discovers by walking upward.
    pub config_files: Vec<RepoPath>,
    /// Every workspace member's manifest. Cargo constructs the whole workspace
    /// graph before building anything and fails if a listed member's manifest is
    /// unreadable.
    pub member_manifests: Vec<RepoPath>,
    /// Full source subtrees: the target crate and its in-repo path dependencies.
    pub source_subtrees: Vec<RepoPath>,
    /// In-repo path dependencies, by dependent crate, for drift classification.
    pub path_dependencies: Vec<(String, RepoPath)>,
}

impl BuildClosure {
    /// Every path the closure names, for the baseline proposal.
    pub fn all_paths(&self) -> Vec<RepoPath> {
        let mut paths: Vec<RepoPath> = self
            .root_manifest
            .iter()
            .cloned()
            .chain(self.config_files.iter().cloned())
            .chain(self.member_manifests.iter().cloned())
            .chain(self.source_subtrees.iter().cloned())
            .collect();
        paths.sort();
        paths.dedup();
        paths
    }

    /// Individual files, which become pins when they fall outside the granted
    /// subtrees.
    pub fn individual_files(&self) -> Vec<RepoPath> {
        let mut files: Vec<RepoPath> = self
            .root_manifest
            .iter()
            .cloned()
            .chain(self.config_files.iter().cloned())
            .chain(self.member_manifests.iter().cloned())
            .collect();
        files.sort();
        files.dedup();
        files
    }
}

/// A parsed manifest, reduced to what the closure needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    pub package_name: Option<String>,
    /// Member patterns from `[workspace] members`.
    pub workspace_members: Vec<String>,
    pub workspace_excludes: Vec<String>,
    /// Whether the manifest declares a `[workspace]` table at all.
    pub is_workspace_root: bool,
    /// Relative path dependencies declared by this manifest.
    pub path_dependencies: Vec<String>,
    /// Whether the package has a build script.
    pub has_build_script: bool,
    /// Whether the package is a proc-macro crate.
    pub is_proc_macro: bool,
}

/// Parses a manifest from TOML text.
pub fn parse_manifest(path: &Path, text: &str) -> Result<Manifest> {
    let value: toml::Value = toml::from_str(text).map_err(|error| SnapshotError::Manifest {
        path: path.to_path_buf(),
        detail: format!("{error}"),
    })?;
    let table = value.as_table().ok_or_else(|| SnapshotError::Manifest {
        path: path.to_path_buf(),
        detail: "top level is not a table".to_owned(),
    })?;

    let package = table.get("package").and_then(toml::Value::as_table);
    let package_name = package
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str)
        .map(str::to_owned);
    let has_build_script = package
        .and_then(|package| package.get("build"))
        .is_some_and(|value| !matches!(value.as_bool(), Some(false)));

    let is_proc_macro = table
        .get("lib")
        .and_then(toml::Value::as_table)
        .and_then(|lib| lib.get("proc-macro").or_else(|| lib.get("proc_macro")))
        .and_then(toml::Value::as_bool)
        .unwrap_or(false);

    let workspace = table.get("workspace").and_then(toml::Value::as_table);
    let string_list = |table: Option<&toml::map::Map<String, toml::Value>>, key: &str| {
        table
            .and_then(|table| table.get(key))
            .and_then(toml::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };

    // Path dependencies appear in three tables and, for workspace inheritance,
    // in the root's `[workspace.dependencies]` too.
    let mut path_dependencies = BTreeSet::new();
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        collect_path_dependencies(table.get(section), &mut path_dependencies);
    }
    if let Some(workspace) = workspace {
        collect_path_dependencies(workspace.get("dependencies"), &mut path_dependencies);
    }
    // Target-specific dependency tables: `[target.'cfg(unix)'.dependencies]`.
    if let Some(targets) = table.get("target").and_then(toml::Value::as_table) {
        for target in targets.values() {
            let Some(target) = target.as_table() else {
                continue;
            };
            for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
                collect_path_dependencies(target.get(section), &mut path_dependencies);
            }
        }
    }

    Ok(Manifest {
        package_name,
        workspace_members: string_list(workspace, "members"),
        workspace_excludes: string_list(workspace, "exclude"),
        is_workspace_root: workspace.is_some(),
        path_dependencies: path_dependencies.into_iter().collect(),
        has_build_script,
        is_proc_macro,
    })
}

fn collect_path_dependencies(section: Option<&toml::Value>, out: &mut BTreeSet<String>) {
    let Some(table) = section.and_then(toml::Value::as_table) else {
        return;
    };
    for value in table.values() {
        if let Some(path) = value
            .as_table()
            .and_then(|dependency| dependency.get("path"))
            .and_then(toml::Value::as_str)
        {
            out.insert(path.to_owned());
        }
    }
}

/// Computes the build closure for `target` within `root`.
pub fn compute(root: &Path, target: &RepoPath) -> Result<BuildClosure> {
    let target_manifest = find_manifest(root, target).ok_or_else(|| SnapshotError::NoManifest {
        path: target.clone(),
    })?;
    let target_dir = target_manifest.parent().unwrap_or_else(RepoPath::root);

    let workspace_root_manifest = find_workspace_root(root, &target_manifest)?;
    let workspace_dir = workspace_root_manifest
        .as_ref()
        .and_then(RepoPath::parent)
        .unwrap_or_else(RepoPath::root);

    let mut closure = BuildClosure {
        root_manifest: workspace_root_manifest.clone(),
        ..BuildClosure::default()
    };

    // Cargo discovers these by walking upward from the crate directory.
    for name in [
        "Cargo.lock",
        ".cargo/config.toml",
        ".cargo/config",
        "rust-toolchain.toml",
        "rust-toolchain",
    ] {
        if let Ok(candidate) = workspace_dir.join(name)
            && candidate.to_host_path(root).exists()
        {
            closure.config_files.push(candidate);
        }
    }

    // Every member's manifest, because cargo builds the whole workspace graph
    // before building anything.
    if let Some(root_manifest) = &workspace_root_manifest {
        let parsed = read_manifest(root, root_manifest)?;
        for pattern in &parsed.workspace_members {
            for member in expand_member_pattern(root, &workspace_dir, pattern) {
                if parsed
                    .workspace_excludes
                    .iter()
                    .any(|excluded| member.as_str().ends_with(excluded.trim_end_matches('/')))
                {
                    continue;
                }
                if let Ok(manifest) = member.join("Cargo.toml")
                    && manifest.to_host_path(root).exists()
                {
                    closure.member_manifests.push(manifest);
                }
            }
        }
    }

    // The target crate's source, plus its transitive in-repo path dependencies.
    let mut source: BTreeSet<RepoPath> = BTreeSet::new();
    let mut dependencies: BTreeMap<String, RepoPath> = BTreeMap::new();
    let mut frontier = vec![target_dir.clone()];
    while let Some(directory) = frontier.pop() {
        if !source.insert(directory.clone()) {
            continue;
        }
        let Ok(manifest_path) = directory.join("Cargo.toml") else {
            continue;
        };
        if !manifest_path.to_host_path(root).exists() {
            continue;
        }
        let parsed = read_manifest(root, &manifest_path)?;
        let dependent = parsed
            .package_name
            .clone()
            .unwrap_or_else(|| directory.to_string());
        for relative in &parsed.path_dependencies {
            let Some(resolved) = resolve_relative(&directory, relative) else {
                continue;
            };
            if !resolved.to_host_path(root).exists() {
                continue;
            }
            dependencies.insert(dependent.clone(), resolved.clone());
            frontier.push(resolved);
        }
    }

    closure.source_subtrees = source.into_iter().collect();
    closure.path_dependencies = dependencies.into_iter().collect();
    closure.member_manifests.sort();
    closure.member_manifests.dedup();
    closure.config_files.sort();
    closure.config_files.dedup();
    Ok(closure)
}

/// The manifest governing `target`: its own, or the nearest one above it.
fn find_manifest(root: &Path, target: &RepoPath) -> Option<RepoPath> {
    let mut current = Some(target.clone());
    while let Some(path) = current {
        if let Ok(candidate) = path.join("Cargo.toml")
            && candidate.to_host_path(root).is_file()
        {
            return Some(candidate);
        }
        // The target may itself be a file, in which case its parent holds the
        // manifest.
        current = path.parent();
    }
    None
}

/// The topmost manifest declaring a `[workspace]` table above the target.
///
/// A single-crate project has none, in which case the crate's own manifest is
/// the root.
fn find_workspace_root(root: &Path, manifest: &RepoPath) -> Result<Option<RepoPath>> {
    let mut best = Some(manifest.clone());
    let mut current = manifest.parent().and_then(|parent| parent.parent());
    while let Some(directory) = current {
        if let Ok(candidate) = directory.join("Cargo.toml")
            && candidate.to_host_path(root).is_file()
        {
            let parsed = read_manifest(root, &candidate)?;
            if parsed.is_workspace_root {
                best = Some(candidate);
            }
        }
        if directory.is_root() {
            break;
        }
        current = directory.parent();
    }
    Ok(best)
}

fn read_manifest(root: &Path, path: &RepoPath) -> Result<Manifest> {
    let host = path.to_host_path(root);
    let text = std::fs::read_to_string(&host)
        .map_err(|error| SnapshotError::io(format!("reading {host:?}"), error))?;
    parse_manifest(&host, &text)
}

/// Expands a `[workspace] members` pattern.
///
/// Cargo supports globs here. Only the shapes projects actually use are
/// supported — a literal path, a trailing `*`, and a trailing `**` — and an
/// unrecognised pattern expands to nothing rather than to everything, which is
/// the fail-closed direction.
fn expand_member_pattern(root: &Path, base: &RepoPath, pattern: &str) -> Vec<RepoPath> {
    let pattern = pattern.trim_end_matches('/');
    if let Some(prefix) = pattern.strip_suffix("/**") {
        let Ok(directory) = base.join(prefix) else {
            return Vec::new();
        };
        return descendants(root, &directory, usize::MAX);
    }
    if let Some(prefix) = pattern.strip_suffix("/*") {
        let Ok(directory) = base.join(prefix) else {
            return Vec::new();
        };
        return descendants(root, &directory, 1);
    }
    if pattern.contains('*') {
        return Vec::new();
    }
    base.join(pattern).into_iter().collect()
}

/// Directories under `directory`, to `depth` levels.
fn descendants(root: &Path, directory: &RepoPath, depth: usize) -> Vec<RepoPath> {
    if depth == 0 {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(directory.to_host_path(root)) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.filter_map(|entry| entry.ok()) {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(child) = directory.join(&name) else {
            continue;
        };
        found.push(child.clone());
        if depth > 1 {
            found.extend(descendants(root, &child, depth.saturating_sub(1)));
        }
    }
    found.sort();
    found
}

/// Resolves a `path = "../other"` dependency against the declaring directory.
fn resolve_relative(base: &RepoPath, relative: &str) -> Option<RepoPath> {
    let mut components: Vec<String> = base.components().map(str::to_owned).collect();
    for part in relative.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                // A dependency that walks above the workspace root is outside
                // the repository entirely and is not resolvable here.
                components.pop()?;
            }
            other => components.push(other.to_owned()),
        }
    }
    if components.is_empty() {
        return Some(RepoPath::root());
    }
    RepoPath::parse(components.join("/")).ok()
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

    fn path(text: &str) -> RepoPath {
        RepoPath::parse(text).unwrap()
    }

    fn write(root: &Path, relative: &str, contents: &str) {
        let target = root.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(target, contents).unwrap();
    }

    #[test]
    fn a_single_crate_project_has_its_own_manifest_as_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "Cargo.toml", "[package]\nname = \"solo\"\n");
        write(root, "Cargo.lock", "version = 4\n");
        write(root, "src/lib.rs", "// lib\n");

        let closure = compute(root, &RepoPath::root()).unwrap();
        assert_eq!(closure.root_manifest, Some(path("Cargo.toml")));
        assert!(closure.config_files.contains(&path("Cargo.lock")));
        assert!(closure.source_subtrees.contains(&RepoPath::root()));
        assert!(closure.member_manifests.is_empty());
    }

    #[test]
    fn a_virtual_workspace_pulls_in_every_member_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n",
        );
        write(root, "Cargo.lock", "version = 4\n");
        write(root, ".cargo/config.toml", "[build]\n");
        write(root, "rust-toolchain.toml", "[toolchain]\n");
        write(root, "crates/a/Cargo.toml", "[package]\nname = \"a\"\n");
        write(root, "crates/a/src/lib.rs", "// a\n");
        write(root, "crates/b/Cargo.toml", "[package]\nname = \"b\"\n");
        write(root, "crates/b/src/lib.rs", "// b\n");

        let closure = compute(root, &path("crates/a")).unwrap();
        assert_eq!(closure.root_manifest, Some(path("Cargo.toml")));
        assert!(
            closure
                .member_manifests
                .contains(&path("crates/b/Cargo.toml")),
            "cargo fails if a listed member's manifest is unreadable: {:?}",
            closure.member_manifests
        );
        assert!(
            !closure.source_subtrees.contains(&path("crates/b")),
            "manifests only, not the other member's sources"
        );
        assert!(closure.config_files.contains(&path(".cargo/config.toml")));
        assert!(closure.config_files.contains(&path("rust-toolchain.toml")));
    }

    #[test]
    fn inherited_workspace_dependencies_are_followed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "Cargo.toml",
            r#"
[workspace]
members = ["crates/app", "crates/shared"]

[workspace.dependencies]
shared = { path = "crates/shared" }
"#,
        );
        write(
            root,
            "crates/app/Cargo.toml",
            "[package]\nname = \"app\"\n\n[dependencies]\nshared = { workspace = true }\n",
        );
        write(root, "crates/app/src/main.rs", "fn main() {}\n");
        write(
            root,
            "crates/shared/Cargo.toml",
            "[package]\nname = \"shared\"\n",
        );
        write(root, "crates/shared/src/lib.rs", "// shared\n");

        let closure = compute(root, &path("crates/app")).unwrap();
        assert!(
            closure
                .member_manifests
                .contains(&path("crates/shared/Cargo.toml")),
            "{:?}",
            closure.member_manifests
        );
    }

    #[test]
    fn nested_path_dependencies_are_followed_transitively() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "Cargo.toml",
            "[workspace]\nmembers = [\"a\", \"b\", \"c\"]\n",
        );
        write(
            root,
            "a/Cargo.toml",
            "[package]\nname = \"a\"\n\n[dependencies]\nb = { path = \"../b\" }\n",
        );
        write(root, "a/src/lib.rs", "// a\n");
        write(
            root,
            "b/Cargo.toml",
            "[package]\nname = \"b\"\n\n[dependencies]\nc = { path = \"../c\" }\n",
        );
        write(root, "b/src/lib.rs", "// b\n");
        write(root, "c/Cargo.toml", "[package]\nname = \"c\"\n");
        write(root, "c/src/lib.rs", "// c\n");

        let closure = compute(root, &path("a")).unwrap();
        assert!(closure.source_subtrees.contains(&path("a")));
        assert!(closure.source_subtrees.contains(&path("b")));
        assert!(
            closure.source_subtrees.contains(&path("c")),
            "a transitive in-repo path dependency needs full source: {:?}",
            closure.source_subtrees
        );
        let dependents: Vec<&str> = closure
            .path_dependencies
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        assert!(dependents.contains(&"a"));
        assert!(dependents.contains(&"b"));
    }

    #[test]
    fn build_dependencies_and_target_specific_tables_are_followed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "Cargo.toml",
            "[workspace]\nmembers = [\"a\", \"helper\", \"unixdep\"]\n",
        );
        write(
            root,
            "a/Cargo.toml",
            r#"
[package]
name = "a"

[build-dependencies]
helper = { path = "../helper" }

[target.'cfg(unix)'.dependencies]
unixdep = { path = "../unixdep" }
"#,
        );
        write(root, "a/src/lib.rs", "// a\n");
        write(root, "helper/Cargo.toml", "[package]\nname = \"helper\"\n");
        write(
            root,
            "unixdep/Cargo.toml",
            "[package]\nname = \"unixdep\"\n",
        );

        let closure = compute(root, &path("a")).unwrap();
        assert!(closure.source_subtrees.contains(&path("helper")));
        assert!(closure.source_subtrees.contains(&path("unixdep")));
    }

    #[test]
    fn manifest_parsing_detects_build_scripts_and_proc_macros() {
        let build_script = parse_manifest(
            Path::new("Cargo.toml"),
            "[package]\nname = \"x\"\nbuild = \"build.rs\"\n",
        )
        .unwrap();
        assert!(build_script.has_build_script);

        let disabled = parse_manifest(
            Path::new("Cargo.toml"),
            "[package]\nname = \"x\"\nbuild = false\n",
        )
        .unwrap();
        assert!(!disabled.has_build_script);

        let proc_macro = parse_manifest(
            Path::new("Cargo.toml"),
            "[package]\nname = \"x\"\n\n[lib]\nproc-macro = true\n",
        )
        .unwrap();
        assert!(proc_macro.is_proc_macro);
    }

    #[test]
    fn a_malformed_manifest_is_an_error_not_an_empty_closure() {
        let error = parse_manifest(Path::new("Cargo.toml"), "this is not toml {{{").unwrap_err();
        assert!(matches!(error, SnapshotError::Manifest { .. }));
    }

    #[test]
    fn a_target_with_no_manifest_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("empty")).unwrap();
        let error = compute(dir.path(), &path("empty")).unwrap_err();
        assert!(matches!(error, SnapshotError::NoManifest { .. }));
    }

    #[test]
    fn member_patterns_expand_conservatively() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("crates/a")).unwrap();
        std::fs::create_dir_all(root.join("crates/b/nested")).unwrap();

        let single = expand_member_pattern(root, &RepoPath::root(), "crates/*");
        assert!(single.contains(&path("crates/a")));
        assert!(!single.contains(&path("crates/b/nested")));

        let deep = expand_member_pattern(root, &RepoPath::root(), "crates/**");
        assert!(deep.contains(&path("crates/b/nested")));

        let literal = expand_member_pattern(root, &RepoPath::root(), "crates/a");
        assert_eq!(literal, vec![path("crates/a")]);

        // An unrecognised glob expands to nothing rather than to everything.
        assert!(expand_member_pattern(root, &RepoPath::root(), "cr*tes/a").is_empty());
    }

    #[test]
    fn relative_dependency_paths_resolve_and_refuse_to_escape() {
        assert_eq!(
            resolve_relative(&path("crates/app"), "../shared"),
            Some(path("crates/shared"))
        );
        assert_eq!(
            resolve_relative(&path("crates/app"), "./nested"),
            Some(path("crates/app/nested"))
        );
        assert_eq!(
            resolve_relative(&path("a"), "../../outside"),
            None,
            "a dependency above the workspace root is not resolvable here"
        );
    }
}

//! Runtime roots as nix closures (D6).
//!
//! A runtime root is identified by its store path, and its closure is what a
//! sandbox binds. Two things live here: reading a closure manifest, and the
//! assertion that `runtimeRoots.workspace` contains no project build toolchain.
//!
//! The authoritative form of that assertion is `checks.runtime-root-workspace`
//! in the flake, which inspects the derivation. This is the same rule expressed
//! over a manifest, so `clyde doctor` can check a configured root and so the
//! rule is unit-testable without a nix build.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clyde_core::task::RuntimeRootKind;

use crate::error::{Result, SandboxError};

/// A runtime root and the closure a sandbox must bind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeRoot {
    pub kind: RuntimeRootKind,
    pub path: PathBuf,
    /// Every store path the root transitively references.
    pub closure: Vec<PathBuf>,
    /// Executable names in the root's `bin` directory.
    pub binaries: Vec<String>,
}

impl RuntimeRoot {
    /// Reads a root from its store path, discovering the closure from the
    /// manifest the flake generates, or falling back to the root alone.
    ///
    /// The manifest is produced by `packages.runtimeRootManifests`; the fallback
    /// exists so a hand-configured root still works, at the cost of binding less
    /// of the store.
    pub fn read(kind: RuntimeRootKind, path: &Path, manifest_dir: Option<&Path>) -> Result<Self> {
        if !path.exists() {
            return Err(SandboxError::RuntimeRoot {
                root: path.to_path_buf(),
                detail: "runtime root store path does not exist".to_owned(),
            });
        }
        // Configuration names a GC root — `/nix/var/nix/gcroots/clyde/rust` —
        // which is a symlink into the store and exists on the host only. What the
        // sandbox binds is the closure, and a closure is store paths, so a
        // program addressed through the GC root is unreachable inside the sandbox
        // however complete that closure is. The store path is also the root's
        // identity (D6); the GC root is an anchor against garbage collection, not
        // a name a task can be handed.
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let closure = match manifest_dir {
            Some(dir) => read_store_paths(&dir.join(kind.name()).join("store-paths"))
                .unwrap_or_else(|| vec![path.clone()]),
            None => vec![path.clone()],
        };
        let binaries = read_binaries(&path.join("bin"));
        Ok(Self {
            kind,
            path,
            closure,
            binaries,
        })
    }

    /// Resolves a program name against this root's `bin` directory.
    ///
    /// Programs come from the runtime root, never from the host `PATH`: the
    /// agent binary is not copied out of the host's path implicitly (Phase 1
    /// deliverable 7).
    pub fn program(&self, name: &str) -> Option<PathBuf> {
        let candidate = self.path.join("bin").join(name);
        candidate.exists().then_some(candidate)
    }

    /// Whether `program` will still resolve once only this closure is bound.
    ///
    /// A `buildEnv` root is a tree of symlinks into other store paths, so a
    /// program that resolves on the host resolves inside the sandbox only if the
    /// path it points at is itself bound. When the closure is just the root —
    /// which is what a missing manifest directory yields — every such symlink
    /// dangles, and the only diagnostic is `bwrap: execvp …: No such file or
    /// directory`, which reads as a missing toolchain rather than as an unbound
    /// closure.
    pub fn resolves_inside_sandbox(&self, program: &Path) -> bool {
        let Ok(target) = program.canonicalize() else {
            return false;
        };
        if target.starts_with(&self.path) {
            return true;
        }
        self.closure.iter().any(|bound| {
            bound
                .canonicalize()
                .is_ok_and(|bound| target.starts_with(&bound))
        })
    }
}

fn read_store_paths(manifest: &Path) -> Option<Vec<PathBuf>> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let paths: Vec<PathBuf> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect();
    (!paths.is_empty()).then_some(paths)
}

fn read_binaries(bin: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(bin) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

/// Programs that must not be reachable from the workspace runtime root.
///
/// The rule is "absent, not forbidden": an agent in that environment cannot run
/// `cargo check` even if it decides to, because the toolchain is not there. That
/// is what makes `run_task` the only path to executing project code (D17).
pub const FORBIDDEN_IN_WORKSPACE_ROOT: [&str; 16] = [
    "cargo",
    "rustc",
    "rustup",
    "node",
    "npm",
    "pnpm",
    "yarn",
    "chromium",
    "firefox",
    "google-chrome",
    "gpg",
    "gpg2",
    "ssh",
    "ssh-agent",
    "docker",
    "podman",
];

/// Tools the workspace runtime root must provide.
pub const REQUIRED_IN_WORKSPACE_ROOT: [&str; 6] = ["sh", "grep", "sed", "find", "jq", "diff"];

/// The result of asserting over a workspace runtime root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRootAssertion {
    pub forbidden_present: Vec<String>,
    pub required_missing: Vec<String>,
}

impl WorkspaceRootAssertion {
    pub fn holds(&self) -> bool {
        self.forbidden_present.is_empty() && self.required_missing.is_empty()
    }

    pub fn render(&self) -> String {
        let mut lines = Vec::new();
        if !self.forbidden_present.is_empty() {
            lines.push(format!(
                "the workspace runtime root contains project build tooling, which would let an agent execute project code without asking Clyde: {}",
                self.forbidden_present.join(", ")
            ));
        }
        if !self.required_missing.is_empty() {
            lines.push(format!(
                "the workspace runtime root is missing tools the agent needs: {}",
                self.required_missing.join(", ")
            ));
        }
        lines.join("; ")
    }
}

/// Asserts the workspace-root rule over a set of binary names.
///
/// Taking names rather than a path keeps the rule pure and testable; callers
/// supply either a real root's binaries or, in a test, a synthetic list.
pub fn assert_workspace_root(binaries: &[String]) -> WorkspaceRootAssertion {
    let present = |name: &str| binaries.iter().any(|binary| binary == name);
    WorkspaceRootAssertion {
        forbidden_present: FORBIDDEN_IN_WORKSPACE_ROOT
            .into_iter()
            .filter(|name| present(name))
            .map(str::to_owned)
            .collect(),
        required_missing: REQUIRED_IN_WORKSPACE_ROOT
            .into_iter()
            .filter(|name| !present(name))
            .map(str::to_owned)
            .collect(),
    }
}

/// Runtime roots by kind, as configured.
#[derive(Debug, Clone, Default)]
pub struct RuntimeRoots {
    roots: BTreeMap<RuntimeRootKind, RuntimeRoot>,
}

impl RuntimeRoots {
    pub fn from_paths(
        paths: &BTreeMap<RuntimeRootKind, PathBuf>,
        manifest_dir: Option<&Path>,
    ) -> Result<Self> {
        let mut roots = BTreeMap::new();
        for (kind, path) in paths {
            roots.insert(*kind, RuntimeRoot::read(*kind, path, manifest_dir)?);
        }
        Ok(Self { roots })
    }

    pub fn get(&self, kind: RuntimeRootKind) -> Option<&RuntimeRoot> {
        self.roots.get(&kind)
    }

    /// Checks the workspace root, if one is configured.
    pub fn check_workspace_root(&self) -> Option<WorkspaceRootAssertion> {
        self.roots
            .get(&RuntimeRootKind::Workspace)
            .map(|root| assert_workspace_root(&root.binaries))
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

    #[test]
    fn a_root_configured_through_a_gc_root_resolves_to_its_store_path() {
        // Configuration names `/nix/var/nix/gcroots/clyde/rust`, which exists on
        // the host and not in the sandbox: the sandbox binds the closure, and a
        // closure is store paths. Handing a task the GC-root path produces
        // `bwrap: execvp …: No such file or directory` however complete the
        // closure is.
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store/clyde-runtime-root-rust");
        std::fs::create_dir_all(store.join("bin")).unwrap();
        std::fs::write(store.join("bin/cargo"), b"#!/bin/sh\n").unwrap();
        let gcroot = dir.path().join("gcroots/rust");
        std::fs::create_dir_all(dir.path().join("gcroots")).unwrap();
        std::os::unix::fs::symlink(&store, &gcroot).unwrap();

        let root = RuntimeRoot::read(RuntimeRootKind::Rust, &gcroot, None).unwrap();
        let program = root.program("cargo").expect("cargo is in the root");
        assert!(
            !program.starts_with(dir.path().join("gcroots")),
            "a task must not be handed the GC-root path: {}",
            program.display()
        );
        assert_eq!(program, store.canonicalize().unwrap().join("bin/cargo"));
        assert!(
            root.resolves_inside_sandbox(&program),
            "the store path is what the closure binds"
        );
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn a_correct_workspace_root_passes() {
        let binaries = names(&[
            "sh", "grep", "sed", "find", "jq", "diff", "rg", "fd", "python3", "patch",
        ]);
        let assertion = assert_workspace_root(&binaries);
        assert!(assertion.holds(), "{}", assertion.render());
    }

    #[test]
    fn a_build_toolchain_in_the_workspace_root_fails_the_assertion() {
        // The test that must fail if someone adds a toolchain: without it, the
        // "absent, not forbidden" property is a hope rather than a rule.
        let binaries = names(&["sh", "grep", "sed", "find", "jq", "diff", "cargo", "rustc"]);
        let assertion = assert_workspace_root(&binaries);
        assert!(!assertion.holds());
        assert_eq!(assertion.forbidden_present, names(&["cargo", "rustc"]));
        assert!(assertion.render().contains("without asking Clyde"));
    }

    #[test]
    fn signing_container_and_browser_tooling_are_forbidden_too() {
        for tool in ["gpg", "ssh", "docker", "podman", "chromium", "node"] {
            let mut binaries = names(&["sh", "grep", "sed", "find", "jq", "diff"]);
            binaries.push(tool.to_owned());
            let assertion = assert_workspace_root(&binaries);
            assert!(!assertion.holds(), "{tool} must be forbidden");
        }
    }

    #[test]
    fn a_root_missing_required_tools_fails() {
        let assertion = assert_workspace_root(&names(&["sh"]));
        assert!(!assertion.holds());
        assert!(assertion.required_missing.contains(&"jq".to_owned()));
    }

    #[test]
    fn reading_a_root_discovers_its_binaries_and_closure() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        for name in ["sh", "jq"] {
            std::fs::write(root.join("bin").join(name), b"#!/bin/sh\n").unwrap();
        }
        let manifests = dir.path().join("manifests");
        std::fs::create_dir_all(manifests.join("workspace")).unwrap();
        std::fs::write(
            manifests.join("workspace").join("store-paths"),
            "/nix/store/a\n/nix/store/b\n",
        )
        .unwrap();

        let read = RuntimeRoot::read(RuntimeRootKind::Workspace, &root, Some(&manifests)).unwrap();
        assert_eq!(read.binaries, names(&["jq", "sh"]));
        assert_eq!(read.closure.len(), 2);
        assert!(read.program("jq").is_some());
        assert!(read.program("cargo").is_none());
    }

    #[test]
    fn a_missing_root_is_an_error_not_an_empty_closure() {
        let error = RuntimeRoot::read(
            RuntimeRootKind::Rust,
            std::path::Path::new("/nix/store/definitely-absent"),
            None,
        )
        .expect_err("a missing root must fail loudly");
        assert!(matches!(error, SandboxError::RuntimeRoot { .. }));
    }

    #[test]
    fn a_root_without_a_manifest_falls_back_to_binding_itself() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let read = RuntimeRoot::read(RuntimeRootKind::Rust, &root, None).unwrap();
        assert_eq!(read.closure, vec![root]);
    }
}

//! Enforcement posture (schema reference: Enforcement posture).
//!
//! Posture is whether the driver can execute project code outside Clyde
//! ([D26]). The builder without the warden is a large, genuine improvement and
//! it is **not** what [D1] promised, and that gap is only safe if it is
//! impossible to be in the weaker mode without being told.
//!
//! Two rules run through this module, and both have tests:
//!
//! - **Derived, never configured.** No configuration key sets posture, and none
//!   can produce [`Posture::Enforcing`] on a host where the bypass exists.
//! - **Reporting only.** Posture participates in no admission decision. Nothing
//!   about observing a bypass may widen or narrow what a host is permitted to
//!   run — the same separation [R10] requires of enclosure detection.
//!
//! Observation is separated from classification for the same reason it is there:
//! [`PostureObservation`] is a plain value a test can construct, and [`derive`]
//! is a pure function over it, so the cases worth diagnosing do not need a host
//! that reproduces them.
//!
//! [D26]: ../../../docs/builder/decisions.md
//! [D1]: ../../../docs/warden/decisions.md
//! [R10]: ../../../docs/builder/decisions.md

use serde::{Deserialize, Serialize};

/// How a driver could execute project code outside Clyde.
///
/// Enumerated rather than free text, so the UX can render each case and a test
/// can assert on it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum BypassReason {
    /// A project build toolchain is reachable on the host's `PATH`. Naming the
    /// program matters: "a toolchain is reachable" sends someone hunting, and
    /// "`cargo` at `/usr/bin/cargo`" does not.
    ToolchainOnHostPath { program: String },
    /// Clyde hosts no actor, so the driver's environment is whatever the host
    /// gives it.
    NoHostedActor,
}

impl BypassReason {
    /// A sentence for `clyde doctor` and mission review.
    pub fn render(&self) -> String {
        match self {
            Self::ToolchainOnHostPath { program } => {
                format!(
                    "a project build toolchain is on PATH ({program}), so project code can be built outside Clyde"
                )
            }
            Self::NoHostedActor => {
                "no agent is hosted, so the driver's environment is the host's".to_owned()
            }
        }
    }
}

/// Whether the driver can execute project code outside Clyde.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "posture")]
pub enum Posture {
    /// The driver cannot reach a project build toolchain outside Clyde. Typed
    /// tasks are the only path to project execution.
    Enforcing,
    /// A driver can bypass the pipeline, and Clyde says specifically how.
    Advisory { reasons: Vec<BypassReason> },
}

impl Posture {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Enforcing => "enforcing",
            Self::Advisory { .. } => "advisory",
        }
    }

    pub fn is_enforcing(&self) -> bool {
        matches!(self, Self::Enforcing)
    }

    /// The bypasses, in order, for rendering.
    pub fn reasons(&self) -> &[BypassReason] {
        match self {
            Self::Enforcing => &[],
            Self::Advisory { reasons } => reasons,
        }
    }
}

impl std::fmt::Display for Posture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Enforcing => f.write_str("enforcing"),
            Self::Advisory { reasons } => {
                let rendered: Vec<String> = reasons.iter().map(BypassReason::render).collect();
                write!(f, "advisory: {}", rendered.join("; "))
            }
        }
    }
}

/// What the host actually looks like, as a value.
///
/// Constructed by probing; consumed by [`derive`]. Keeping them apart is what
/// lets the interesting cases — a host with no toolchain, a deployment with a
/// hosted agent — be tested on a machine that is neither.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PostureObservation {
    /// Project build programs found on the host's `PATH`, in the order looked
    /// for. Empty when none is reachable.
    pub toolchain_on_path: Vec<String>,
    /// Whether Clyde hosts the actor, so that its environment — and therefore
    /// the absence of a toolchain from it — is Clyde's to guarantee.
    pub hosts_the_actor: bool,
}

/// Classifies an observation.
///
/// Pure, total, and the only place posture is decided.
pub fn derive(observation: &PostureObservation) -> Posture {
    let mut reasons: Vec<BypassReason> = observation
        .toolchain_on_path
        .iter()
        .map(|program| BypassReason::ToolchainOnHostPath {
            program: program.clone(),
        })
        .collect();
    if !observation.hosts_the_actor {
        reasons.push(BypassReason::NoHostedActor);
    }
    if reasons.is_empty() {
        Posture::Enforcing
    } else {
        Posture::Advisory { reasons }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn a_host_with_no_toolchain_and_a_hosted_actor_is_enforcing() {
        assert_eq!(
            derive(&PostureObservation {
                toolchain_on_path: Vec::new(),
                hosts_the_actor: true,
            }),
            Posture::Enforcing
        );
    }

    #[test]
    fn a_toolchain_on_path_is_advisory_and_the_program_is_named() {
        // "A toolchain is reachable" sends someone hunting; the program and its
        // path do not.
        let posture = derive(&PostureObservation {
            toolchain_on_path: vec!["cargo at /usr/bin/cargo".to_owned()],
            hosts_the_actor: true,
        });
        assert!(!posture.is_enforcing());
        assert!(posture.to_string().contains("/usr/bin/cargo"), "{posture}");
    }

    #[test]
    fn hosting_nothing_is_a_bypass_on_its_own() {
        // A clean host with no toolchain is still advisory when Clyde hosts
        // nothing: a driver with network access can fetch a toolchain, so
        // toolchain absence is only durable when egress is controlled too.
        let posture = derive(&PostureObservation {
            toolchain_on_path: Vec::new(),
            hosts_the_actor: false,
        });
        assert_eq!(
            posture,
            Posture::Advisory {
                reasons: vec![BypassReason::NoHostedActor]
            }
        );
    }

    #[test]
    fn every_bypass_is_reported_rather_than_only_the_first() {
        // Fixing one and still being advisory, with no explanation of why, is
        // exactly the confusion D26 exists to prevent.
        let posture = derive(&PostureObservation {
            toolchain_on_path: vec!["cargo".to_owned(), "rustc".to_owned()],
            hosts_the_actor: false,
        });
        assert_eq!(posture.reasons().len(), 3, "{posture}");
    }

    #[test]
    fn enforcing_cannot_be_reached_while_any_bypass_is_observed() {
        // The property that makes posture worth reporting: there is no
        // combination of inputs that reports the stronger mode while a bypass
        // exists. Configuration is not among the inputs at all.
        for toolchain in [Vec::new(), vec!["cargo".to_owned()]] {
            for hosts in [true, false] {
                let observed = PostureObservation {
                    toolchain_on_path: toolchain.clone(),
                    hosts_the_actor: hosts,
                };
                let bypassable = !observed.toolchain_on_path.is_empty() || !hosts;
                assert_eq!(
                    derive(&observed).is_enforcing(),
                    !bypassable,
                    "{observed:?}"
                );
            }
        }
    }
}

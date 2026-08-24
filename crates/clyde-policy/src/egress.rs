//! Egress profile ordering and allowlist resolution.
//!
//! Lease derivation and escalation checks need a partial order on profiles.
//! `model-api` and `rust-registry` are deliberately **incomparable**: neither is
//! a superset of the other, so a lease holding one cannot derive a child holding
//! the other. That requires an escalation evaluated against the mission, not a
//! derivation (network egress model: profile ordering).

use std::cmp::Ordering;
use std::collections::BTreeSet;

use clyde_core::classification::{EgressProfile, HostName};

/// The built-in registry hosts.
///
/// Configuration may narrow this set; a repository can never add to it (D14).
pub const DEFAULT_REGISTRY_HOSTS: [&str; 3] = ["crates.io", "static.crates.io", "index.crates.io"];

/// The built-in registry host set as parsed values.
///
/// Malformed entries are dropped rather than panicking; the constant is covered
/// by a test that asserts all three parse.
pub fn default_registry_hosts() -> BTreeSet<HostName> {
    DEFAULT_REGISTRY_HOSTS
        .into_iter()
        .filter_map(|host| HostName::parse(host).ok())
        .collect()
}

/// Partial order over egress profiles, using the built-in registry host set.
///
/// `Some(Less)` means the first profile is strictly narrower. `None` means the
/// two are incomparable, which is a denial rather than a fallback.
pub fn egress_profile_order(a: &EgressProfile, b: &EgressProfile) -> Option<Ordering> {
    egress_profile_order_with(a, b, &default_registry_hosts())
}

/// Partial order over egress profiles against a specific registry host set.
///
/// The host set matters for exactly one rule: `rust-registry < custom(H)` holds
/// only when every registry host is in `H`, and which hosts count as registry
/// hosts is configuration.
pub fn egress_profile_order_with(
    a: &EgressProfile,
    b: &EgressProfile,
    registry_hosts: &BTreeSet<HostName>,
) -> Option<Ordering> {
    use EgressProfile::*;
    match (a, b) {
        // Reflexive cases.
        (None, None) | (ModelApi, ModelApi) | (RustRegistry, RustRegistry) | (Broker, Broker) => {
            Some(Ordering::Equal)
        }
        (Custom { hosts: left }, Custom { hosts: right }) => subset_order(left, right),

        // `none` is below every sandbox profile.
        (None, ModelApi | RustRegistry | Custom { .. }) => Some(Ordering::Less),
        (ModelApi | RustRegistry | Custom { .. }, None) => Some(Ordering::Greater),

        // `model-api` is below any custom list. The credential injection is
        // host-side, so a custom list that names the model hosts is strictly
        // wider in reachability terms.
        (ModelApi, Custom { .. }) => Some(Ordering::Less),
        (Custom { .. }, ModelApi) => Some(Ordering::Greater),

        // `rust-registry < custom(H)` iff the registry hosts are all in H.
        (RustRegistry, Custom { hosts }) => {
            if registry_hosts.is_subset(hosts) {
                Some(Ordering::Less)
            } else {
                Option::None
            }
        }
        (Custom { hosts }, RustRegistry) => {
            if registry_hosts.is_subset(hosts) {
                Some(Ordering::Greater)
            } else {
                Option::None
            }
        }

        // The documented incomparable pair.
        (ModelApi, RustRegistry) | (RustRegistry, ModelApi) => Option::None,

        // `broker` is not a sandbox profile: it names the broker's own egress,
        // outside any sandbox, so it is incomparable with every sandbox profile
        // rather than sitting above or below them.
        (Broker, _) | (_, Broker) => Option::None,
    }
}

fn subset_order(left: &BTreeSet<HostName>, right: &BTreeSet<HostName>) -> Option<Ordering> {
    match (left.is_subset(right), right.is_subset(left)) {
        (true, true) => Some(Ordering::Equal),
        (true, false) => Some(Ordering::Less),
        (false, true) => Some(Ordering::Greater),
        (false, false) => None,
    }
}

/// Whether `requested` is no wider than `ceiling` (derivation rule 4).
///
/// Fails closed: incomparable profiles are not permitted.
pub fn is_no_wider_than(requested: &EgressProfile, ceiling: &EgressProfile) -> EgressComparison {
    match egress_profile_order(requested, ceiling) {
        Some(Ordering::Less | Ordering::Equal) => EgressComparison::NoWider,
        Some(Ordering::Greater) => EgressComparison::Wider,
        None => EgressComparison::Incomparable,
    }
}

/// The outcome of an egress ceiling check.
///
/// `Incomparable` is distinct from `Wider` because the two produce different
/// diagnostics and different remedies: one needs a narrower request, the other
/// needs an escalation evaluated against the mission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressComparison {
    NoWider,
    Wider,
    Incomparable,
}

/// Resolves a profile to the concrete host allowlist the proxy enforces.
///
/// `none` resolves to an empty set, and the caller must not bind a proxy socket
/// at all: the absence of the socket *is* the absence of egress.
pub fn resolve_allowlist(
    profile: &EgressProfile,
    registry_hosts: &BTreeSet<HostName>,
    model_api_hosts: &BTreeSet<HostName>,
) -> BTreeSet<HostName> {
    match profile {
        EgressProfile::None => BTreeSet::new(),
        EgressProfile::ModelApi => model_api_hosts.clone(),
        EgressProfile::RustRegistry => registry_hosts.clone(),
        // The broker's egress is not proxied through a sandbox socket, so it has
        // no sandbox allowlist.
        EgressProfile::Broker => BTreeSet::new(),
        EgressProfile::Custom { hosts } => hosts.clone(),
    }
}

/// Ports a profile permits.
///
/// Only 443 is permitted for every profile: an allowlist entry names a host, and
/// permitting arbitrary ports on an allowlisted host would widen the destination
/// scope the approval prompt described.
pub fn permits_port(profile: &EgressProfile, port: u16) -> bool {
    match profile {
        EgressProfile::None | EgressProfile::Broker => false,
        EgressProfile::ModelApi | EgressProfile::RustRegistry | EgressProfile::Custom { .. } => {
            port == 443
        }
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

    fn hosts(names: &[&str]) -> BTreeSet<HostName> {
        names
            .iter()
            .filter_map(|name| HostName::parse(name).ok())
            .collect()
    }

    fn custom(names: &[&str]) -> EgressProfile {
        EgressProfile::Custom {
            hosts: hosts(names),
        }
    }

    #[test]
    fn default_registry_hosts_all_parse() {
        assert_eq!(default_registry_hosts().len(), DEFAULT_REGISTRY_HOSTS.len());
    }

    #[test]
    fn none_is_below_every_sandbox_profile() {
        for profile in [
            EgressProfile::ModelApi,
            EgressProfile::RustRegistry,
            custom(&["example.test"]),
        ] {
            assert_eq!(
                egress_profile_order(&EgressProfile::None, &profile),
                Some(Ordering::Less)
            );
            assert_eq!(
                egress_profile_order(&profile, &EgressProfile::None),
                Some(Ordering::Greater)
            );
        }
        assert_eq!(
            egress_profile_order(&EgressProfile::None, &EgressProfile::None),
            Some(Ordering::Equal)
        );
    }

    #[test]
    fn model_api_and_rust_registry_are_incomparable() {
        // The load-bearing case: a lease holding one cannot derive a child
        // holding the other.
        assert_eq!(
            egress_profile_order(&EgressProfile::ModelApi, &EgressProfile::RustRegistry),
            None
        );
        assert_eq!(
            egress_profile_order(&EgressProfile::RustRegistry, &EgressProfile::ModelApi),
            None
        );
        assert_eq!(
            is_no_wider_than(&EgressProfile::RustRegistry, &EgressProfile::ModelApi),
            EgressComparison::Incomparable
        );
    }

    #[test]
    fn model_api_is_below_any_custom_list() {
        assert_eq!(
            egress_profile_order(&EgressProfile::ModelApi, &custom(&["unrelated.test"])),
            Some(Ordering::Less)
        );
    }

    #[test]
    fn rust_registry_is_below_custom_only_when_the_hosts_are_covered() {
        let covering = custom(&[
            "crates.io",
            "static.crates.io",
            "index.crates.io",
            "extra.test",
        ]);
        assert_eq!(
            egress_profile_order(&EgressProfile::RustRegistry, &covering),
            Some(Ordering::Less)
        );
        let partial = custom(&["crates.io"]);
        assert_eq!(
            egress_profile_order(&EgressProfile::RustRegistry, &partial),
            None,
            "a custom list missing a registry host is not comparable"
        );
    }

    #[test]
    fn custom_lists_order_by_subset() {
        let small = custom(&["a.test"]);
        let big = custom(&["a.test", "b.test"]);
        let disjoint = custom(&["c.test"]);
        assert_eq!(egress_profile_order(&small, &big), Some(Ordering::Less));
        assert_eq!(egress_profile_order(&big, &small), Some(Ordering::Greater));
        assert_eq!(egress_profile_order(&small, &small), Some(Ordering::Equal));
        assert_eq!(egress_profile_order(&small, &disjoint), None);
    }

    #[test]
    fn broker_is_incomparable_with_every_sandbox_profile() {
        for profile in [
            EgressProfile::None,
            EgressProfile::ModelApi,
            EgressProfile::RustRegistry,
            custom(&["a.test"]),
        ] {
            assert_eq!(egress_profile_order(&EgressProfile::Broker, &profile), None);
            assert_eq!(egress_profile_order(&profile, &EgressProfile::Broker), None);
        }
        assert_eq!(
            egress_profile_order(&EgressProfile::Broker, &EgressProfile::Broker),
            Some(Ordering::Equal)
        );
    }

    #[test]
    fn ceiling_check_fails_closed_on_incomparable() {
        assert_eq!(
            is_no_wider_than(&EgressProfile::None, &EgressProfile::ModelApi),
            EgressComparison::NoWider
        );
        assert_eq!(
            is_no_wider_than(&EgressProfile::ModelApi, &EgressProfile::None),
            EgressComparison::Wider
        );
        assert_eq!(
            is_no_wider_than(&EgressProfile::Broker, &EgressProfile::ModelApi),
            EgressComparison::Incomparable
        );
    }

    #[test]
    fn allowlist_resolution_is_empty_for_none() {
        let registry = default_registry_hosts();
        let model = hosts(&["api.anthropic.com"]);
        assert!(
            resolve_allowlist(&EgressProfile::None, &registry, &model).is_empty(),
            "profile none must resolve to no reachable host"
        );
        assert_eq!(
            resolve_allowlist(&EgressProfile::RustRegistry, &registry, &model),
            registry
        );
        assert_eq!(
            resolve_allowlist(&EgressProfile::ModelApi, &registry, &model),
            model
        );
        assert!(resolve_allowlist(&EgressProfile::Broker, &registry, &model).is_empty());
    }

    #[test]
    fn only_https_is_permitted() {
        assert!(permits_port(&EgressProfile::RustRegistry, 443));
        assert!(!permits_port(&EgressProfile::RustRegistry, 80));
        assert!(!permits_port(&EgressProfile::RustRegistry, 22));
        assert!(!permits_port(&EgressProfile::None, 443));
    }

    #[test]
    fn ordering_is_antisymmetric_where_defined() {
        let profiles = [
            EgressProfile::None,
            EgressProfile::ModelApi,
            EgressProfile::RustRegistry,
            EgressProfile::Broker,
            custom(&["a.test"]),
            custom(&["a.test", "b.test"]),
        ];
        for a in &profiles {
            for b in &profiles {
                match (egress_profile_order(a, b), egress_profile_order(b, a)) {
                    (Some(Ordering::Less), Some(Ordering::Greater))
                    | (Some(Ordering::Greater), Some(Ordering::Less))
                    | (Some(Ordering::Equal), Some(Ordering::Equal))
                    | (None, None) => {}
                    (left, right) => panic!("{a} vs {b} is asymmetric: {left:?} / {right:?}"),
                }
            }
        }
    }
}

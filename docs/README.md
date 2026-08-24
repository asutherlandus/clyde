# Clyde Next Design Documents

This directory contains the design document set for the Clyde Next redesign.

Clyde Next is intended to be a **least-privilege development system for Rust and full-stack applications with strong support for agentic coding**.

The design assumes:
- untrusted code may execute during dependency install, build, test, and code generation
- credentials must be **brokered, not mounted**
- build, test, sign, and publish must be **separate environments with different authority**
- humans and agents act as **actors** within **missions**
- work happens through **tasks** evaluated by **policy** and run in controlled **environments**
- the workspace environment supports low-authority code-manipulation scripts, and deliberately lacks the project build/test toolchain
- Clyde hosts the agent process inside that workspace environment, so typed tasks are the only path to project execution

## Recommended reading order

### 1. Foundations

1. [Problem Statement and Threat Model](problem-statement-threat-model.md)
2. [Terminology](terminology.md)
3. [Requirements](requirements.md)
4. [Competitive Alternatives](competitive-alternatives.md)
5. [High-Level Design](high-level-design.md)

### 2. Core execution and interaction model

6. [Mission and Capability Lease Model](mission-lease-model.md)
7. [Task Policy Matrix](task-policy-matrix.md)
8. [Sequence Flows and Interaction Scenarios](sequence-flows.md)

### 3. Implementation planning

9. [Component Architecture](component-architecture.md)
10. [Technology and Library Choices](technology-choices.md)
11. [MVP Implementation Roadmap](mvp-implementation-roadmap.md)

### 4. Implementation decisions and specs

12. [Implementation Decision Log](decisions.md) — binding decisions for Phases 0-4, with rationale, consequences, and open questions
13. [Schema Reference](schema-reference.md) — entities, state machines, invariants, persistence
14. [Agent Integration and the Workspace Environment](agent-and-workspace-environment.md)
15. [Network Egress Model](network-egress-model.md)
16. Phase specs: [0](phase-0-foundations.md) · [1](phase-1-mission-lease-approval.md) · [2](phase-2-execution-and-isolation.md) · [3](phase-3-dependency-resolution.md) · [4](phase-4-credential-broker.md)

**Start here if you are implementing:** [decisions.md](decisions.md), then the spec for the phase you are working on.

## Document map

### Strategy and framing
- [Problem Statement and Threat Model](problem-statement-threat-model.md)
- [Terminology](terminology.md)
- [Competitive Alternatives](competitive-alternatives.md)

### Product and system design
- [Requirements](requirements.md)
- [High-Level Design](high-level-design.md)
- [Mission and Capability Lease Model](mission-lease-model.md)
- [Task Policy Matrix](task-policy-matrix.md)
- [Sequence Flows and Interaction Scenarios](sequence-flows.md)

### Implementation design
- [Component Architecture](component-architecture.md)
- [Technology and Library Choices](technology-choices.md)
- [MVP Implementation Roadmap](mvp-implementation-roadmap.md)

### Implementation decisions and specs
- [Implementation Decision Log](decisions.md)
- [Schema Reference](schema-reference.md)
- [Agent Integration and the Workspace Environment](agent-and-workspace-environment.md)
- [Network Egress Model](network-egress-model.md)
- [Phase 0: Foundations](phase-0-foundations.md)
- [Phase 1: Mission, Lease, and Approval Core](phase-1-mission-lease-approval.md)
- [Phase 2: Snapshot Execution and Isolation Backends](phase-2-execution-and-isolation.md)
- [Phase 3: Separate Dependency Resolution](phase-3-dependency-resolution.md)
- [Phase 4: Credential Broker and Brokered Git Push](phase-4-credential-broker.md)

## Short summary

The Clyde Next design is centered on this model:
- an **actor** works on a **mission**
- the **workspace** is the mutable project being changed
- a **task** is the unit of work being requested
- **policy** decides whether and how that task may run
- an **environment** determines where the work happens

In practice, Clyde uses:
- a **workspace environment** for editing and low-authority code manipulation, which also hosts the agent
- a **research environment** for web search, documentation reading, and research artifacts
- a **build environment** for fetch, build, test, and other project execution
- a **broker environment** for push, sign, publish, and similar privileged actions

[Terminology](terminology.md) defines the shared vocabulary used across the document set.

## Repository context

This branch contains design and implementation-planning documents only; the legacy Docker-based implementation has been removed. Implementation of Phase 0 has not started.

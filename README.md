![CLYDE](clyde_banner.png)

# CLYDE

Clyde is being redesigned as a **least-privilege development system for Rust and full-stack applications with strong support for agentic coding**.

This branch is focused on the **clean-slate architecture and design work** for that next version.

In the simplest terms: **an actor works on a mission in a workspace, asks to run a task, policy decides whether and how it may run, and the task runs in an appropriate environment.**

## Status

This branch contains the **Clyde Next design and implementation-planning document set** in `docs/`. The legacy Docker-based implementation has been removed.

Implementation decisions for Phases 0-4 are settled and recorded in [docs/decisions.md](docs/decisions.md); each phase has a spec. No code has been written yet — Phase 0 is the next step.

The design direction assumes:
- untrusted code may execute during build, test, install, and codegen
- credentials must be **brokered, not mounted**
- network should be **denied by default**
- build, test, sign, and publish must be **separate environments with different authority**
- humans and agents act as **actors** within **missions**
- work happens through **tasks** evaluated by **policy** and run in controlled **environments**
- the workspace environment supports low-authority code-manipulation scripts, and project build/test toolchains stay outside it
- Clyde hosts the coding agent inside that workspace environment, so typed tasks are the only path to project execution

## Clyde Next document set

Start here:

1. [Problem Statement and Threat Model](docs/problem-statement-threat-model.md)
2. [Terminology](docs/terminology.md)
3. [Requirements](docs/requirements.md)
4. [Competitive Alternatives](docs/competitive-alternatives.md)
5. [High-Level Design](docs/high-level-design.md)

Detailed design:

- [Mission and Capability Lease Model](docs/mission-lease-model.md)
- [Task Policy Matrix](docs/task-policy-matrix.md)
- [Sequence Flows and Interaction Scenarios](docs/sequence-flows.md)
- [Component Architecture](docs/component-architecture.md)
- [Technology and Library Choices](docs/technology-choices.md)
- [MVP Implementation Roadmap](docs/mvp-implementation-roadmap.md)

Implementation decisions and specs:

- [Implementation Decision Log](docs/decisions.md)
- [Schema Reference](docs/schema-reference.md)
- [Agent Integration and the Workspace Environment](docs/agent-and-workspace-environment.md)
- [Network Egress Model](docs/network-egress-model.md)
- Phase specs: [0](docs/phase-0-foundations.md) · [1](docs/phase-1-mission-lease-approval.md) · [2](docs/phase-2-execution-and-isolation.md) · [3](docs/phase-3-dependency-resolution.md) · [4](docs/phase-4-credential-broker.md)

## Design summary

Clyde Next is intended to be:

- a **trusted local or self-hosted control plane**
- a **secure execution system** for hostile build/test/install workflows
- a **developer-facing workspace** for humans and coding agents
- a **credential broker** for git, signing, and publish operations
- an **artifact and audit system** connecting all trust boundaries

Core design ideas:

- **missions** define bounded goals
- **actors** work within mission limits
- the **workspace** is mutable and used for authoring
- **tasks** are the units of work Clyde controls
- **policy** decides whether and how tasks may run
- **environments** separate editing, research, build/test execution, and privileged external actions
- **leases**, **snapshots**, and **brokers** enforce those boundaries

## Near-term focus

The MVP (Phases 0-4) targets one strong end-to-end workflow:

- a mission delegated to an agent hosted in a Clyde-managed workspace environment
- snapshot-based `rust.check` and `rust.test.unit` with no network and no credentials
- bubblewrap isolation first, Firecracker microVMs before any network-bearing task
- approval-gated, registry-only dependency resolution through a Clyde egress proxy
- brokered `git.push` with no credential reachable from any agent or build sandbox

Full-stack, browser testing, signing, and publishing follow the MVP.

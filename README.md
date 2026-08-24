![CLYDE](clyde_banner.png)

# CLYDE

Clyde is being redesigned as a **least-privilege development system for Rust and full-stack applications with strong support for agentic coding**.

This branch is focused on the **clean-slate architecture and design work** for that next version.

In the simplest terms: **an actor works on a mission in a workspace, asks to run a task, policy decides whether and how it may run, and the task runs in an appropriate environment.**

## Status

This repository currently contains:

- the **legacy Docker-based implementation** in `bin/` and `docker/`
- a new **Clyde Next design document set** in `docs/`

The design direction assumes:
- untrusted code may execute during build, test, install, and codegen
- credentials must be **brokered, not mounted**
- network should be **denied by default**
- build, test, sign, and publish must be **separate environments with different authority**
- humans and agents act as **actors** within **missions**
- work happens through **tasks** evaluated by **policy** and run in controlled **environments**
- the workspace environment may support low-authority code-manipulation scripts, but project build/test toolchains remain outside that environment

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

## Legacy implementation

The existing Docker-based Clyde implementation remains in the repository as a reference point during the redesign, but it does **not** represent the target architecture for Clyde Next.

## Near-term focus

The current design work is primarily focused on:

- Rust build/test isolation
- full-stack dependency and browser-test isolation
- agent/human interaction design
- mission/lease/task policy model
- sandbox and broker architecture for an MVP

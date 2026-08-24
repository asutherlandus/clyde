# Clyde Next Design Documents

This directory contains the design document set for the Clyde Next redesign.

Clyde Next is intended to be a **least-privilege development system for Rust and full-stack applications with strong support for agentic coding**.

The design assumes:
- untrusted code may execute during dependency install, build, test, and code generation
- credentials must be **brokered, not mounted**
- build, test, sign, and publish must be **separate trust domains**
- humans and agents should work through **missions, leases, typed tasks, and approvals**

## Recommended reading order

### 1. Foundations

1. [Problem Statement and Threat Model](problem-statement-threat-model.md)
2. [Requirements](requirements.md)
3. [Competitive Alternatives](competitive-alternatives.md)
4. [High-Level Design](high-level-design.md)

### 2. Core execution and interaction model

5. [Mission and Capability Lease Model](mission-lease-model.md)
6. [Task Policy Matrix](task-policy-matrix.md)
7. [Sequence Flows and Interaction Scenarios](sequence-flows.md)

### 3. Implementation planning

8. [Component Architecture](component-architecture.md)
9. [Technology and Library Choices](technology-choices.md)
10. [MVP Implementation Roadmap](mvp-implementation-roadmap.md)

## Document map

### Strategy and framing
- [Problem Statement and Threat Model](problem-statement-threat-model.md)
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

## Short summary

The Clyde Next design is centered on these ideas:
- **missions** define bounded goals and autonomy envelopes
- **leases** grant temporary scoped authority to agents and sub-agents
- **typed tasks** define how untrusted code executes under policy
- **snapshots** separate mutable editing from isolated execution
- **brokers** separate credentials and publish authority from build/test environments
- **approvals** happen at boundary crossings rather than every inner-loop iteration

## Repository context

The repository still contains the legacy Docker-based Clyde implementation in `bin/` and `docker/`, but those components are not the target architecture for Clyde Next. The documents in this directory define the redesign direction.

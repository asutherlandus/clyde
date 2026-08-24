# Clyde Next: Problem Statement and Threat Model

## Purpose

Clyde needs a clean-sheet redesign for a new problem: creating a least-privilege development environment for Rust code and modern full-stack applications while preserving strong support for agentic coding.

The core concern is not only host isolation from a developer shell. It is isolation from the software being developed, its build tools, its dependency graph, and any agent-directed commands that may execute that software. In this model, project code and build steps must be treated as potentially hostile.

## Problem Statement

Modern development environments routinely collapse four very different trust domains into one session:

1. **Editing and planning**
2. **Dependency resolution and package download**
3. **Build and test execution**
4. **Credentialed operations such as git push, signing, and publishing**

In a typical local setup, all four domains share:
- the same filesystem view
- the same network access
- the same user identity
- the same long-lived caches
- the same SSH, GPG, GitHub, and cloud credentials

That design is convenient, but it is fundamentally mismatched to the current supply-chain threat landscape.

For Rust and full-stack applications in particular, ordinary development workflows routinely execute untrusted code before a human ever reviews it. This creates a path for malicious dependencies, compromised registries, or agent-generated commands to exfiltrate credentials, tamper with artifacts, or pivot into the host environment.

## Why Rust Is a Special Problem

Rust has a strong memory-safety story, but its build security model is weak. Arbitrary code execution during development is not an edge case; it is a built-in feature of the ecosystem.

### Rust-specific execution surfaces

The following can execute arbitrary code during normal development:
- `build.rs`
- procedural macros
- custom test binaries
- doctests
- linker wrappers and external toolchain hooks
- `cargo xtask` and project-specific helper commands
- code generators invoked from cargo workflows

As a result, commands commonly perceived as low risk are not low risk:
- `cargo check`
- `cargo build`
- `cargo test`
- `cargo metadata` in some workflows
- IDE and language-server driven background analysis

A crate update, proc macro, or build script can run attacker-controlled code during compilation without requiring the developer to execute a clearly suspicious command.

## Why Full-Stack Development Is Also a High-Risk Environment

Modern full-stack development adds additional code execution paths and network complexity.

### Frontend and Node ecosystem risks

The following routinely execute code from dependencies or project configuration:
- `npm install`, `pnpm install`, `yarn install`
- `postinstall` and lifecycle scripts
- bundler plugins for Vite, Webpack, Rollup, or esbuild
- test runners and their plugins
- ORM/codegen tools such as Prisma
- browser automation hooks
- lint, formatting, and build plugins

### Backend and integration-test risks

Full-stack repos also tend to require:
- local databases and queues
- service-to-service communication
- API tokens for third-party integrations
- browser sessions and cookies
- staging or preview deployments

This pressure often leads teams to grant broad network access and mount real credentials into local development environments. That convenience directly undermines least privilege.

## Security Problem to Solve

Clyde should assume that any of the following may be compromised:
- a source repository
- a transitive dependency
- a package registry account
- a crate, npm package, or git dependency
- a build script or proc macro
- a test harness or browser automation hook
- a generated command from an AI coding agent
- a long-lived cache or shared tool state

The system must therefore prevent untrusted development tasks from obtaining unnecessary access to:
- SSH credentials
- GPG signing keys
- GitHub or cloud tokens
- the host filesystem
- unrelated projects
- unrestricted network egress
- signing and publishing capabilities

## Tension: Central Control vs Agentic Development

A clean-sheet Clyde also needs to resolve an important product and security tension.

On one hand, Clyde should be the developer's single point of contact with the system. That implies:
- one place where policy is enforced
- one place where credentials are brokered
- one place where approvals are requested
- one place where audit and provenance are recorded

On the other hand, effective agentic development depends on autonomous inner loops such as:
- repeated edit / compile / test iteration
- spawning specialized sub-agents
- parallel work across frontend, backend, tests, or docs
- retries and exploration without constant human interruption

These goals appear to conflict if "single point of contact" is interpreted to mean that only the human can initiate each action, or that every loop iteration requires a fresh approval.

That interpretation would make the system too slow and would push users back toward broad shell access and long-lived privileged workspaces.

The design challenge is therefore not merely sandboxing code execution. It is creating a model where:
- Clyde remains the sole authority and mediation layer
- humans can delegate bounded autonomy to agents
- agents can iterate quickly inside pre-authorized limits
- sub-agents can be created without multiplying privilege
- approval is required for boundary crossings, not for every safe inner-loop action

This implies a key design direction for the solution:

> Clyde should be the sole policy and authority interface, but not the sole actor. Agents and sub-agents must be able to operate through Clyde under constrained, time-bounded, auditable capability grants.

## Threat Model

### Primary attacker goals

An attacker who gains code execution through the development workflow may attempt to:
- exfiltrate SSH, GPG, GitHub, cloud, or package-publishing credentials
- tamper with the source tree or generated artifacts
- modify release outputs before signing or publishing
- move laterally into the host system or other projects
- persist in caches, build directories, or long-lived containers
- call external command-and-control infrastructure over the network
- abuse an AI agent's execution privileges to expand access

### Key attack vectors

#### 1. Malicious Rust dependency
A transitive crate contains a `build.rs` or proc macro that reads mounted secrets, searches the workspace for credentials, or writes modified source and artifacts.

#### 2. Malicious JavaScript package
An npm package runs a lifecycle script during install or build and exfiltrates environment variables, browser cookies, or API tokens.

#### 3. Compromised repository
A repository intentionally includes hostile build tooling, test hooks, or convenience scripts that are likely to be executed by the developer or agent.

#### 4. Agent-assisted escalation
An AI agent with broad shell access is induced to run commands that expose secrets, expand mounts, or push tampered code upstream.

#### 5. Cache poisoning and persistence
A compromised build step writes malicious state into shared caches, tool directories, or persistent volumes so future tasks inherit attacker-controlled inputs.

#### 6. Credential pivot during publish
A build step that can also access signing or git-push credentials can replace artifacts or commits immediately before publication.

## Assets to Protect

The most important assets are:
- SSH authentication capability
- GPG or other signing key capability
- Git hosting tokens and session state
- cloud provider credentials
- package registry credentials
- browser cookies and authenticated sessions
- the host machine and user account
- the integrity of source, build outputs, and release artifacts
- provenance records showing how an artifact was produced

## Trust Boundaries

A secure design must separate at least these major parts of the system:

1. **Control plane**: trusted local supervisor enforcing policy
2. **Workspace environment**: code reading, planning, editing, and low-authority code manipulation
3. **Build environment**: build, test, codegen, and package installation
4. **Broker environment**: git push, signing, publishing, and token issuance
5. **Artifact layer**: immutable inputs, caches, outputs, and provenance

These parts should communicate through explicit, typed, auditable interfaces rather than shared shells, mounted home directories, or forwarded credential sockets.

## Security Objectives

The new Clyde should be designed to achieve the following objectives:

### Prevent credential exposure
Untrusted build and test steps must not receive raw SSH keys, GPG key material, cloud credentials, browser sessions, or general-purpose agent sockets.

### Minimize filesystem visibility
Tasks should receive only the repository subtree and outputs required for that step, ideally via immutable snapshots plus isolated scratch space.

### Deny network by default
Compilation, testing, and code generation should run without network unless a narrowly scoped task profile explicitly allows it.

### Separate build from publish
No environment that executes untrusted project code should also be able to sign, push, or publish artifacts.

### Preserve agentic usefulness
The system must still support iterative editing, build/test feedback, and automated code assistance, but through policy-controlled task execution rather than unrestricted shell access.

### Support audit and provenance
Each task should produce enough metadata to understand what inputs, capabilities, and outputs were involved.

## Non-Goals

This redesign does not need to guarantee:
- perfect defense against every kernel or hypervisor escape
- fully reproducible builds for every supported ecosystem on day one
- seamless compatibility with every existing local-dev habit
- zero-friction use of arbitrary external services from untrusted tasks

The goal is meaningful least-privilege isolation with a practical developer experience, not total elimination of all risk.

## Desirable Characteristics of a Solution

A suitable solution should have the following high-level characteristics:
- **task-based isolation** instead of one long-lived all-powerful dev container
- **separate environments** for editing, execution, and privileged external actions
- **ephemeral sandboxes** for untrusted build and test steps
- **immutable snapshots** for task inputs and controlled artifact outputs
- **network denied by default**, with explicit destination-scoped exceptions
- **brokered credentials** rather than mounted secrets or forwarded agents
- **typed operations** such as fetch, build, test, sign, and publish, each with a fixed policy
- **auditability and provenance** for task execution and artifact movement
- **strong support for agentic coding** through a policy-aware execution API

In short, Clyde should treat source code execution, dependency resolution, credential use, and publishing as separate parts of the system connected only by explicit policy-controlled interfaces.

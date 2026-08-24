# Clyde Next: Terminology

## Purpose

This document defines the canonical domain terms used throughout the Clyde Next design documents.

It is intentionally simple. The goal is to establish a small shared vocabulary before introducing lower-level architecture or implementation terms.

## Core model

Clyde can be described in one sentence:

> An **actor** works on a **mission** in a **workspace**, asks to run a **task**, **policy** decides whether and how it may run, and the task runs in an appropriate **environment**.

These are the core terms of the system.

## Canonical definitions

## Mission
A **mission** is a bounded goal with scope and limits.

A mission defines:
- the objective
- the repo or path scope
- the allowed kinds of work
- the time and resource limits
- the approval and escalation boundaries

Short version:
> mission = bounded goal and autonomy envelope

## Actor
An **actor** is a human or agent doing work.

Examples:
- human developer
- primary coding agent
- sub-agent

An actor works within a mission and does not receive ambient authority.

Short version:
> actor = worker

## Workspace
A **workspace** is the mutable project files being edited.

It is where:
- code is read
- files are modified
- changes are prepared before other work runs

The workspace is for authoring, not the default place for hostile project execution.

Short version:
> workspace = editable project files

## Task
A **task** is a unit of work requested by an actor.

Examples:
- run a code rewrite helper script
- fetch dependencies
- build
- test
- push

A task is a named kind of work that Clyde can reason about and control.

Short version:
> task = unit of work

## Task request
A **task request** is an actor asking Clyde to perform a task.

Example:
> request `rust.build` for `backend/`

Short version:
> task request = ask Clyde to do a task

## Task run
A **task run** is one concrete execution of a task.

The same task may be run many times during iterative work.

Example:
- task: `rust.test.unit`
- run: one specific execution against one specific workspace state or snapshot

Short version:
> task run = one execution of a task

## Task result
A **task result** is the outcome of a task run.

A task result may include:
- success, failure, or cancellation
- exit status
- summary or diagnostics
- references to logs
- references to artifacts
- the policy and environment used

Short version:
> task result = outcome of a task run

## Policy
A **policy** is the rules that decide whether and how a task may run.

Policy answers questions like:
- can this actor do this task?
- in this mission?
- in which environment?
- with what network access?
- with what credential access?
- with or without approval?

Short version:
> policy = rules

## Environment
An **environment** is the execution context where a task runs.

This is the simplest top-level term for where work happens.

Short version:
> environment = where work runs

## Environment subtypes

At the domain level, Clyde uses four main environment types.

## Workspace environment
A **workspace environment** is a low-authority environment for editing and code-manipulation work.

Use it for:
- direct file edits
- patch application
- codemods
- batch edits
- structured rewrites
- one-off helper scripts that operate on source files

Properties:
- uses the live mutable workspace
- scoped to allowed repo paths
- may write within allowed workspace scope
- no raw credentials
- no broad network by default
- no full project build toolchain

At the task level, these activities can all be treated as forms of **workspace editing** rather than as a separate top-level kind of work.

## Research environment
A **research environment** is a read-oriented environment for searching external sources and producing research outputs.

Use it for:
- web searching
- reading documentation
- gathering references
- producing notes, summaries, or other research artifacts

Properties:
- read-only access to the workspace or a scoped subset of it
- may have network access according to policy
- no raw credentials by default
- does not modify the workspace directly
- may produce artifacts such as notes, link collections, or summaries

## Build environment
A **build environment** is an isolated environment for project code execution.

Use it for:
- dependency fetch
- build
- test
- code generation
- other project execution that may run hostile code

Properties:
- isolated from the live workspace
- usually snapshot-based
- used for project execution
- treated as hostile when project code may execute

## Broker environment
A **broker environment** is a privileged environment for push, sign, publish, and similar external actions.

Use it for:
- git push
- signing
- publishing
- other scoped authority-bearing operations

Properties:
- performs privileged external effects
- does not expose raw credentials to normal task execution
- does not run untrusted project code in the same context

## Supporting terms

These terms matter, but they build on the core model above.

## Lease
A **lease** is a time-bounded scoped grant that allows a specific actor to act within a mission.

Short version:
> mission = envelope
> lease = active grant

## Approval
An **approval** is an explicit human decision allowing or denying a boundary crossing.

## Boundary crossing
A **boundary crossing** is a requested action outside the current approved mission or lease envelope.

## Snapshot
A **snapshot** is an immutable copy of workspace inputs used by a build environment.

## Broker
A **broker** is a privileged service that performs an authority-bearing action without exposing raw credentials to the actor or task.

## Artifact
An **artifact** is a stored input or output that moves between environments through explicit channels.

Examples:
- snapshot
- dependency bundle
- build output
- logs
- traces
- signature

A task result describes what happened.
Artifacts are the durable outputs produced by a task run.

## Clyde-specific mappings

## Workspace-edit execution boundary
This is an implementation rule behind the workspace environment.

At the domain level, the important meaning is:

> workspace editing may use helper computation, but the workspace environment must stay low-authority and must not quietly become a full dev container.

Concretely, it must:
- be separate from build/test/fetch environments
- include text and code manipulation tooling
- not include the full project build toolchain
- keep project build, test, install, and publish work outside workspace editing

## Safe inner loop
The **safe inner loop** is the set of repeated actions an actor can perform inside an approved mission and lease without asking the human every time.

Typical examples:
- edit files
- make a task request
- use workspace-edit helper scripts or transforms
- run build or test tasks already allowed by policy
- inspect task results and retry

## Canonical term guidance

Prefer these terms in high-level documents:
- **mission**
- **actor**
- **workspace**
- **task**
- **policy**
- **environment**
- **workspace environment**
- **research environment**
- **build environment**
- **broker environment**

De-emphasize these terms in foundational explanations:
- plane
- runtime
- runtime class
- trust domain
- task family
- separate edit-execution mode as a primary user-facing term

Preferred simplifications:
- say **environment** instead of **plane** or **runtime** when explaining the core model
- say **task** instead of **typed operation** when possible
- say **policy** as the decision rules, not as a vague philosophy

## Request and execution verbs

Prefer these verbs in the design docs:
- **request**: an actor asks Clyde to do something
- **run**: Clyde executes a task
- **spawn**: Clyde creates a sub-agent
- **approve / deny**: a human authorizes or rejects a boundary crossing

Guidance:
- actors **request** tasks
- Clyde **runs** tasks
- task runs produce **task results**
- task runs may produce **artifacts**
- actors **spawn** sub-agents, not tasks

## Minimal glossary

| Term | Short meaning |
|---|---|
| Mission | Bounded goal and autonomy envelope |
| Actor | Human or agent doing work |
| Workspace | Editable project files |
| Task | Unit of work |
| Task request | Ask Clyde to do a task |
| Task run | One execution of a task |
| Task result | Outcome of a task run |
| Policy | Rules that decide what is allowed |
| Environment | Where work runs |
| Workspace environment | Low-authority editing and code-manipulation environment |
| Research environment | Read-oriented environment for web search and research artifacts |
| Build environment | Isolated project-execution environment |
| Broker environment | Privileged external-action environment |
| Lease | Active scoped grant inside a mission |
| Approval | Human decision at a boundary crossing |
| Snapshot | Immutable input set for build execution |
| Broker | Service that performs privileged actions safely |
| Artifact | Stored input or output passed between environments |

## Summary

The simplest stable model for Clyde is:
- **Mission**: why the work exists
- **Actor**: who is doing it
- **Workspace**: what is being changed
- **Task**: what work is requested
- **Policy**: what is allowed
- **Environment**: where it runs

For execution flow:
- an actor makes a **task request**
- Clyde starts a **task run**
- the run returns a **task result**
- the run may produce **artifacts**

Everything else should build on top of that model, not replace it.
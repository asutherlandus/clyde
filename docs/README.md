# Clyde Next Design Documents

Clyde Next is a **least-privilege development system for Rust and full-stack applications with strong support for agentic coding**.

The design assumes untrusted code executes during dependency install, build, test, and code generation; that credentials must be brokered rather than mounted; and that build, test, sign, and publish need separate environments with different authority.

## Two products

The MVP ships as two products with separate scopes and separate exit criteria ([D23](builder/decisions.md#d23-the-mvp-splits-into-the-builder-and-the-warden)). Knowing which one a document is about is the most useful thing to hold while reading.

**[The builder](builder/)** — the sandboxed build pipeline. Snapshots, access baselines, the dependency inventory, typed build tasks, the fetch/compile split, brokered authority, and audit. Driven by a human at a terminal, by CI, or by a coding agent the developer already runs. It has no notion of an agent process.

**[The warden](warden/)** — the agent harness. Hosting a coding agent inside a Clyde-managed workspace environment, with the model-API egress path and sub-agent derivation.

They separate because they defend against different attackers. The builder's is the **dependency**: actively hostile, inside the workload, needing an *execution* boundary. The warden's is the **agent**: usually careless rather than hostile, the driver rather than the workload, needing an *authority* boundary.

The warden is a product on top of the builder, not a prerequisite for it. The builder alone cannot stop a driver executing project code outside Clyde entirely; it reports that as `advisory` [posture](terminology.md#posture) rather than leaving it implied ([D26](builder/decisions.md#d26-enforcement-posture-is-explicit-reported-and-recorded)).

> These two products were called **Part 1** and **Part 2** in earlier drafts and in commit history: the builder is Part 1, the warden is Part 2. The warden is named for what it holds custody of — the driver's **authority** — not for confining a prisoner; the agent it hosts is semi-trusted, careless rather than hostile. It is deliberately not called "the sandbox", because *a sandbox* is the isolation mechanism both products use.

## Reading order

**If you want to run it**, take [builder/quick-start.md](builder/quick-start.md) — it states what the walkthrough proves and what it does not — then [builder/manual-builds.md](builder/manual-builds.md) for the compile/test loop it leaves you in.

**If you are implementing**, start with [builder/decisions.md](builder/decisions.md) — [D23](builder/decisions.md#d23-the-mvp-splits-into-the-builder-and-the-warden) first, since it changes what several earlier decisions are decisions *about* — then the phase you are working on in [builder/roadmap.md](builder/roadmap.md).

**If you are new to the design**, read in this order:

1. [Threat model](threat-model.md) — the problem, the attacker, and which product answers which vector
2. [Security model](security-model.md) — what is enforced against it, by what mechanism, and how strongly
3. [Terminology](terminology.md) — the shared vocabulary
4. [builder/design.md](builder/design.md) — goal, requirements, architecture, technology, and flows
5. [builder/tasks-and-policy.md](builder/tasks-and-policy.md) — what a task is and what policy decides
6. [warden/design.md](warden/design.md) — what hosting the agent adds

## The document set

### Shared
| Document | Contents |
|---|---|
| [terminology.md](terminology.md) | canonical vocabulary for both products |
| [threat-model.md](threat-model.md) | problem statement, attack vectors, assets, trust boundaries, non-goals |
| [security-model.md](security-model.md) | what is enforced: the chain of custody, the seven layers, how strongly each control holds, the trusted computing base, what the model does not enforce, and the prior art each control instantiates |
| [competitive-alternatives.md](competitive-alternatives.md) | survey of third-party alternatives, and the gap they leave |

### [builder/](builder/) — the build pipeline
| Document | Contents |
|---|---|
| [quick-start.md](builder/quick-start.md) | the walkthrough: host to one sandboxed `cargo check` under an approved mission and a confirmed baseline |
| [manual-builds.md](builder/manual-builds.md) | driving compile and test by hand: the operator loop, scoping a run, dependencies, reading failures |
| [microvm-bring-up.md](builder/microvm-bring-up.md) | running a build task inside a Firecracker guest: host setup, building the guest from the flake, and what to read when one does not boot |
| [design.md](builder/design.md) | product goal, requirements, architecture, components, technology stack, interaction flows |
| [mission-and-lease.md](builder/mission-and-lease.md) | the authority model: missions, leases, approvals, escalation, budgets |
| [tasks-and-policy.md](builder/tasks-and-policy.md) | task catalog, trust and runtime classes, access baselines, the egress model |
| [schema.md](builder/schema.md) | entities, state machines, invariants, persistence |
| [decisions.md](builder/decisions.md) | the binding decisions, refinements, and open questions |
| [roadmap.md](builder/roadmap.md) | phase order, deliverables, exit criteria, risks, testing strategy |

### [warden/](warden/) — the agent harness
| Document | Contents |
|---|---|
| [design.md](warden/design.md) | the workspace environment, editing authority, the actor API, the model-API channel, sub-agents |
| [decisions.md](warden/decisions.md) | the four decisions that only apply when Clyde hosts the agent |
| [spec.md](warden/spec.md) | deliverables, security properties, exit criteria |

## Repository context

The MVP slice is implemented; see the [root README](../README.md) for what has and has not been exercised, and [builder/roadmap.md](builder/roadmap.md) for how the remaining work divides.

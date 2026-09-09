# Competitive Alternatives

A survey of third-party systems that overlap with the Clyde Next direction: secure development environments, isolated code execution, and support for agentic coding. Background rather than design; it belongs to neither product.

This document covers *products* Clyde could have been built on. For the *models* its controls instantiate — capability security, the reference monitor, SLSA, and the rest — see [Lineage and prior art](security-model.md#lineage-and-prior-art).

## Conclusion

Several strong partial alternatives exist. Based on public documentation, **no off-the-shelf system appears to combine all of**:

- strong isolation for untrusted build and test execution
- fine-grained separation of fetch, build, test, sign, and publish
- brokered SSH/GPG/cloud credentials rather than mounted secrets
- policy-aware support for agentic coding
- practical support for both Rust and full-stack workflows
- a developer-friendly local or self-hosted control plane

The public alternatives fall into three buckets: **workspace products** that are convenient but too coarse-grained; **sandbox runtimes** that are secure primitives but not full developer systems; and **reproducibility or build tools** that help determinism but not credential-safe hostile execution.

## Comparison

Evaluated on: agentic support, isolation strength against hostile build scripts and proc macros, fetch/build separation, credential brokerage, egress control, Rust and full-stack fit, and self-hostability.

| Product | Category | Agentic | Isolation model | Network control | Credential model | Step-level isolation | Main gap vs the target design |
|---|---|---|---|---|---|---|---|
| **Coder** | Self-hosted AI dev platform | Yes | Cloud workspaces on VMs/pods/containers | Partial | Strong for model/API governance; unclear for build credentials | Partial | Workspace-centric, not task-by-task least privilege; no first-class build-time credential broker |
| **Daytona** | AI sandbox runtime | Yes | Sandboxes with dedicated kernel, filesystem, and network stack | Yes | Partial | Partial | A secure execution substrate, not a full workflow and credential architecture |
| **E2B** | AI sandbox cloud | Yes | Isolated cloud sandboxes | Partial | Weak/unclear | Partial | An execution primitive; little support for policy-rich credential brokerage |
| **GitHub Codespaces** | Hosted remote dev env | Partial | Docker container on a VM | Partial | Secrets, but not broker-first | No | Long-lived workspace, not a sequence of least-privilege sandboxes |
| **Gitpod** | Remote dev workspaces | Partial | Remote workspaces/containers | Partial | Env-var and workspace oriented | No | Environment consistency, not credential mediation or build-stage decomposition |
| **DevPod** | Devcontainer launcher | Partial | Devcontainers on local/remote backends | Partial | **Syncs** git and Docker credentials for convenience | No | Credential sync is close to the opposite of broker-first least privilege |
| **Dagger** | Build/workflow engine | Partial | Containerized typed workflow execution | Yes | Secrets API | Yes | Best match for pipeline decomposition; not an interactive dev or agent environment |
| **Devbox** | Reproducible local env | No | Nix-backed shell on the host | No/limited | Host-oriented | No | Not a security boundary against hostile build code |
| **devenv** | Reproducible env + services | Partial | Nix-based local environments and tasks | No/limited | Integrates with secrets systems | Partial | Strong task and service UX; still host-local, not hostile-code sandboxing |
| **Docker AI sandboxes / Claude container wrappers** | Agent container wrapper | Yes | Container boundary | Partial | Often mounts or forwards dev credentials | No | Single long-lived environment; too coarse for the supply-chain threat model |

## Closest candidates

If the goal were to avoid building from scratch, the systems worth a deep evaluation are:

1. **Coder** — the strongest platform story for self-hosted AI development governance, if Clyde became a remote development platform rather than a task-isolation system
2. **Daytona** — the strongest secure sandbox/runtime story for AI-generated code, and the most credible third-party building block for the execution layer
3. **E2B** — a clean programmatic sandbox API for agents
4. **Dagger** — the strongest typed workflow and artifact-flow story

A composite is conceivable: Coder or a devcontainer product for the workspace, Daytona or E2B for untrusted execution, Dagger for the task graph, and a custom credential broker and policy engine on top. That still leaves substantial threat-model-specific design and integration work, and it is the work this document set describes.

Each candidate should be assessed against five questions: can it isolate `build.rs` and proc macros with no network and no secrets; can it broker git/SSH/GPG rather than mounting credentials; can it stage dependency fetch separately from compile and test; can it preserve agent usability while enforcing policy; and can it run fully locally or self-hosted.

## Sources

Public documentation and READMEs only, which may not capture private roadmaps or enterprise-only features: GitHub Codespaces docs; `github.com/coder/coder` and `coder.com/docs`; `github.com/loft-sh/devpod`; `github.com/daytonaio/daytona` and `daytona.io/docs`; `github.com/e2b-dev/E2B` and `e2b.dev/docs`; `github.com/dagger/dagger`; `github.com/jetify-com/devbox`; `github.com/cachix/devenv`.

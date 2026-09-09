# BRP orchestration and Git workflow

This document is the repository-local authority for planning, assigning, reviewing, and integrating
BRP work. It supplements the codec invariants in `AGENTS.md`; it never relaxes them.

Do not invoke or follow the machine-wide `codex-project-workflow:release-git-flow` skill for BRP.
Its permanent `develop` and release-only `main` model conflicts with the branching architecture
below. This narrow exception does not apply to `codex-project-workflow:project-context-optimizer`
or to any unrelated globally required skill.

## Sources of truth

- GitHub Issues and pull requests describe active work, dependencies, ownership, evidence, and
  current state. A fresh orchestrator must be able to reconstruct the program from them.
- Repository documents describe stable rules and accepted technical knowledge. Do not copy a
  transient issue board or PR queue into this repository.
- `docs/FORMAT.md` is normative for the bitstream. Experimental code and reports cannot change it.
- `docs/EXPERIMENTS.md` and `docs/ROADMAP.md` contain orchestrator-synthesised conclusions, not
  concurrent worker notes. Each run has one report under `docs/experiments/runs/`; one bounded
  issue and PR may add multiple related run reports.

## Roles

### Orchestrator

The orchestrator:

- owns the dependency graph and reads the current GitHub state before assigning work;
- decides which issues may run in parallel and which base and target branch each issue uses;
- recommends a model and reasoning effort and writes the complete prompt for every worker;
- reviews PR state, evidence, and CI, and requests an independent high-capability review when risk
  warrants it;
- chooses merge order and the rebase or conflict-resolution strategy;
- synthesises accepted research into the canonical experiment record;
- controls changes to `docs/ROADMAP.md` and final conclusions in `docs/EXPERIMENTS.md`; and
- is the integration authority for temporary format branches.

The orchestrator should not take ordinary worker implementation. Its durable state belongs in
Issues, PRs, and repository documentation rather than conversation context.

### Worker

A worker:

- receives one bounded issue and a complete prompt;
- uses one isolated branch and worktree for that issue;
- changes only the necessary scope and runs the required verification;
- records commands, results, evidence, and limitations;
- opens or updates a PR against the assigned target branch; and
- never merges or independently updates global roadmap or experiment conclusions.

## Branch architecture

### `main`

`main` is the stable accepted state. It must remain buildable and test-clean, but it is not reserved
only for releases. Safe infrastructure, experiment and corpus tooling, documentation, benchmarks,
and accepted `brp-lab` research may merge directly to it through PRs.

There is no permanent `develop` branch.

### Temporary format integration branches

When several related changes are intended for a new bitstream version, the orchestrator creates a
temporary branch such as `format/v8` from accepted `main`. Individual format issues use isolated
branches and PRs targeting that integration branch. No experimental result may silently become
normative.

The integration branch may merge to `main` only after the complete version has all of the following:

- an accepted ADR;
- matching normative `docs/FORMAT.md` changes;
- matching encode, decode, and analysis changes;
- a format version bump and updated golden tests;
- malformed-input coverage;
- passing workspace tests and clippy; and
- the benchmark or experiment evidence required by the decision.

Delete the temporary integration branch after acceptance.

### Worker branches and worktrees

Use one issue -> one branch/worktree -> one PR. Prefer these short categories:

- `infra/...`
- `research/...`
- `corpus/...`
- `benchmark/...`
- `format/...`
- `fix/...`

The orchestrator records the base and PR target in the issue. Normal infrastructure and research
start from `main`; accepted format-version work starts from its assigned `format/vN` branch. Workers
do not work directly on shared branches and do not merge their own PRs.

## Assignment and integration loop

1. The orchestrator refreshes Issues, PRs, CI, branch state, and dependencies.
2. It chooses one ready issue and writes a worker prompt containing the exact goal, relevant paths,
   non-obvious invariants, base and target branches, model and reasoning, verification, evidence
   requirements, and stopping point.
3. The worker creates an isolated worktree, implements only that issue, verifies it, records the
   evidence, pushes, and opens or updates the PR.
4. The orchestrator checks the diff, CI, issue acceptance criteria, and conflicts. Decoder trust
   boundaries, methodology, cross-cutting changes, and format work receive independent review at
   the level in the routing table below.
5. The orchestrator decides whether to rebase, resolve in a dedicated integration branch, request
   changes, or merge. The worker never resolves a conflict by discarding another accepted change.
6. After merge, close the issue, remove the worker branch/worktree when safe, refresh dependants,
   and assign only newly unblocked work.

## Model routing

Choose by risk and verifiability, not prestige. This is operational guidance and should be updated
as models change.

| Work | Preferred model and reasoning |
|---|---|
| Architecture, bitstream/version decisions, difficult integration conflicts, final interpretation of conflicting evidence, critical acceptance gates | GPT-5.6 Sol, max |
| Important independent review, decoder/security correctness, methodology review, complex cross-cutting work | GPT-5.6 Sol, high |
| Normal Rust implementation, experiment harnesses, corpus and benchmark tooling, CI, medium refactors | GPT-5.6 Terra, medium or high |
| Mechanical edits, boilerplate, straightforward tests, report plumbing, issue/document maintenance | GPT-5.6 Luna, low or medium |
| Low-risk bounded work with mechanically verifiable output | A suitable local coding model may be used |

API price ratios are not evidence for ChatGPT Plus weekly-limit ratios. Route from task risk and
observed limits instead.

Local models are never final authorities for bitstream changes, ADR decisions, decoder
trust-boundary or security decisions, benchmark interpretation, or merges. Their output receives
the same tests and PR review as any other worker output.

### Local feasibility on the orchestration host

Snapshot on 2026-09-09: Windows 11 Pro, Ryzen 9 3950X (16 cores / 32 threads), 64 GiB RAM, and an
RTX 2080 Ti with 11 GiB VRAM. Local coding inference is practical for bounded work, but model choice
matters:

- Ollama's [`qwen2.5-coder:14b`](https://ollama.com/library/qwen2.5-coder:14b) image is about
  9 GB and is the sensible first GPU-resident trial, using a modest context so the KV cache also
  fits.
- Ollama's [`qwen3-coder:30b`](https://ollama.com/library/qwen3-coder:30b) image is about 19 GB. It
  fits system RAM and can run with partial GPU offload, but it will not fit wholly in 11 GiB VRAM
  and is likely too slow for interactive orchestration.
- [`qwen3-coder-next`](https://ollama.com/library/qwen3-coder-next) is about 52 GB and is not a
  practical fit for this GPU; avoid treating system-RAM capacity alone as acceptable interactive
  performance.

Do not install a model as part of ordinary repository work. Benchmark a candidate on a small,
mechanically scored BRP task before adding it to worker routing.

## Pull request gates

Every PR links its issue, names its target branch, states whether the bitstream changes, lists exact
verification, supplies relevant experiment evidence, identifies ADR or documentation implications,
and records known limitations. Normal PR CI runs formatting, workspace tests, and clippy; mass
corpus work and performance benchmarks run explicitly outside normal CI.

Workers must not merge. The orchestrator may merge only when the target branch, dependencies,
review level, CI, and evidence all match the issue. For format integration, the stricter checklist
above is mandatory.

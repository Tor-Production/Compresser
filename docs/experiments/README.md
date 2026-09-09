# Experiment workflow

This directory holds reviewed, reproducible BRP experiment records without making concurrent
workers edit the canonical `docs/EXPERIMENTS.md`.

## Two questions that must stay separate

Every run declares exactly one intent:

1. **Historical reproduction:** can the original experiment be reproduced under its historical
   commit, codec, corpus, configuration, and measurement method?
2. **Current revalidation:** does the historical conclusion still hold on the current codec and the
   current mass corpus?

These are different claims. Do not combine their samples, configurations, summaries, or conclusions
in one result. If an issue asks both questions, produce separately identified runs and compare them
only after each has its own result.

## Scheduling

- Performance benchmarks must run alone on a machine: no concurrent CPU- or GPU-heavy build,
  benchmark, model inference, corpus conversion, or timing job.
- Size-only corpus experiments may run concurrently when they do not make timing claims and do not
  contend for a resource that can change their result.
- A mixed size-and-timing tool is a timing job even if only one table reports throughput.
- Warm-up, repetition, interleaving, and process-affinity choices belong in the run metadata when
  timing matters.

## One report per run

Every run has exactly one intent and exactly one report. Create
`runs/YYYY-MM-DD-issue-NNN-intent-short-slug.md` from `TEMPLATE.md`. One bounded research issue,
branch, and PR may contain multiple related runs and therefore multiple reports. In particular, an
issue that asks for both historical reproduction and current revalidation submits at least two
reports through its one PR; it never combines those intents in one report.

A research PR does not edit `docs/EXPERIMENTS.md`, `docs/ROADMAP.md`, or `docs/FORMAT.md`. The last
file may be edited only by an issue explicitly assigned to an accepted format-version branch.

Before merge, reviewers may request corrections in the report. After merge, treat it as immutable:
record a material correction or a new measurement in a new report whose `Supersedes` entry points
to the old one. Do not edit the old report to add a future-facing reverse link; repository search,
the issue, and the superseding report provide that relationship. Keep raw output out of Git when it
is large, but give it a stable location, provenance, and checksum or content identifier where
practical.

Only the orchestrator may synthesise reviewed reports into canonical conclusions in
`docs/EXPERIMENTS.md` and, when priorities genuinely change, `docs/ROADMAP.md`.

## Required reproducibility metadata

Every report records:

- issue and experiment identifier;
- run intent (`historical-reproduction` or `current-revalidation`);
- commit SHA and branch;
- codec and format version;
- corpus root, version or manifest, and provenance;
- exact pipeline and configuration;
- exact command and relevant environment variables;
- sample count, exclusions, and failures;
- whether timing was measured;
- machine information when timing matters;
- raw and summary result locations;
- conclusion; and
- known limitations.

Do not report only a corpus mean where a distribution or source split is material. Preserve source
and imaging-pipeline provenance as first-class dimensions.

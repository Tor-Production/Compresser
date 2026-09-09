# Experiment: <short title>

```yaml
experiment_id: "issue-NNN/run-name"
issue: "https://github.com/Yurii-Tor/Compresser/issues/NNN"
intent: "historical-reproduction | current-revalidation"
commit: "<full SHA>"
branch: "<branch>"
codec_version: "<crate/revision description>"
format_version: <integer or not-applicable>
corpus:
  root: "<path or artifact URI>"
  version: "<manifest, snapshot, or content id>"
  provenance: "<sources and acquisition notes>"
configuration: "<complete pipeline/configuration>"
command: "<exact command>"
sample_count: <integer>
excluded_or_failed: "<count and reasons>"
timing_measured: <true-or-false>
machine: "<required for timing; otherwise not-applicable>"
raw_results: "<stable path or artifact URI>"
summary_results: "<stable path or artifact URI>"
```

## Question

State one falsifiable question. This file records one run with exactly one intent. When an issue
requests both historical reproduction and current revalidation, create separate report files for
the separate runs; the bounded issue and PR may contain both reports.

## Method

Describe corpus selection, preprocessing, controls, repetitions, scheduling, aggregation, and every
departure from the historical or current standard configuration.

## Results

Include distributions and source-level splits where they affect interpretation. Link raw output;
do not transcribe only favourable rows.

## Conclusion

Answer the stated question as supported, refuted, or inconclusive. Keep observations separate from
interpretation.

## Known limitations

List threats to reproduction, representativeness, timing validity, and generalisation.

## Review and supersession

- Reviewed by: `<reviewer or PR>`
- Supersedes: `<report path or none>`

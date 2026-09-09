## Issue and integration

- Linked issue: <!-- Closes #NNN or Relates to #NNN -->
- Target branch: <!-- main or format/vN -->
- Bitstream changes: <!-- no / yes; if yes, link the accepted ADR and format-version issue -->

## Change

<!-- What changed, and why is this the smallest scope that closes the issue? -->

## Verification

<!-- List exact commands and results. -->

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo test --workspace --locked`
- [ ] `cargo clippy --workspace --locked -- -D warnings`

## Evidence and documentation

- Benchmark or experiment evidence: <!-- link report/artifact, or explain why not applicable -->
- Documentation/ADR implications: <!-- paths/links, or none -->
- Known limitations: <!-- explicit limitations, or none known -->

## Worker declaration

- [ ] I worked only on the linked bounded issue.
- [ ] I did not change canonical roadmap/experiment conclusions unless the issue assigned that work.
- [ ] I will not merge this PR; integration belongs to the orchestrator.

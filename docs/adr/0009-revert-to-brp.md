# ADR 0009 — Revert to BRP

**Status:** accepted (2026-09-05)

## Context

ADR 0008 renamed the format to Cimilarity and was enacted the same day: magic `43 49 4D 1A`,
extension `.cim`, crates `cim-*`, CLI `cim`. It recorded two costs as knowingly accepted — that the
name describes the whole category of lossless image compression rather than this codec, and that a
deliberate misspelling of a common word costs search visibility and has to be spelled out loud
every time it is said.

On reflection those costs were judged to outweigh the gain.

## Decision

The format is BRP again, in every respect. This was done as a `git revert` of the rename commit, so
the two cancel exactly and anything left behind is deliberate rather than missed.

The `version` byte stays at 4. It never moved, which is what made both the rename and this revert
an edit rather than a migration.

## Why this is cheap

ADR 0008's own argument, applied symmetrically. While no file of this format exists outside this
repository, its name is a constant, four bytes in each golden fixture and a pass over the
documents. That is what made the rename affordable, and it is what makes undoing it affordable.
The argument expires the moment the format has users, and it expires for renames and for reverts on
the same day.

Files written by the one build that emitted `43 49 4D 1A` are rejected as `BadMagic`, because the
magic check accepts only `42 52 50 1A`. That is the correct outcome and needs no migration path.

## What remains true

The observation that started ADR 0008 is not withdrawn: BRP stands for Block Range Packing, which
is one stage of three, and by measured contribution not the most important one. The name is
inherited from version 1 and has been outgrown. Nothing here settles that; it only says that
Cimilarity was not the answer.

If the question is reopened, ADR 0008 is the better statement of it than this document, and its
verdict on what a format name should be — precise, unambiguous, hard to misspell, and pointed at
the thing rather than the category — is the constraint any future candidate has to satisfy.

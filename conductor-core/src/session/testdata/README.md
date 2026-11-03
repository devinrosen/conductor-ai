# Test fixtures

## `transcript-cc-2.1.317.jsonl`

A small, hand-anonymised Claude Code session transcript, shaped after a real
**Claude Code 2.1.317** transcript (RFC 021 §9, #3165). All text, ids and paths are
placeholders. It is shaped to exercise the ingester:

- two prompts (`promptId` `P1`, `P2`); assistant entries have no `promptId` and reach it
  through `parentUuid`, including through an `attachment` entry;
- 9 assistant entries that are 8 messages (`m1` repeats, once per content block);
- a rejected tool use (`toolUseResult: "User rejected tool use"`) followed by a
  `[Request interrupted by user for tool use]` entry;
- two `cost-state` entries (only the latest counts), the latest with a side-call model
  (`claude-haiku-5-5`) so the totals exceed the per-message sums;
- a non-JSON line, an `api-request-blob`, and unknown entry types.

The transcript format is internal to Claude Code and undocumented; if a Claude Code update
changes it, refresh this fixture and note the new version here.

# RFC 001: Communication Obligation Tracker

**Status:** Closed (out of scope — describes a separate tool, not conductor-ai)
**Date:** 2026-03-10
**Author:** Devin

---

## Problem

During async and synchronous communication (Slack huddles, Google Meet, etc.), people ask you for things: a code review, a doc, a decision, a follow-up message. These requests get lost in the noise of context-switching. There's no structured way to:

- Capture the request at the moment it's made
- Track it through to completion
- Respond via the same channel the request came from

## Proposed Solution

A personal communication obligation tracker that queues requests, tracks their status, and enables responding back through the original communication channel (Slack message, email, PR comment, etc.).

### Core Concepts

- **Request** — a captured obligation: who asked, what they asked for, via which channel, and when
- **Channel** — the communication medium (Slack, email, GitHub PR, etc.)
- **Response** — the artifact or reply delivered back through the original channel

---

## Is This conductor-ai or a Separate Tool?

**Recommendation: separate tool** that uses the conductor workflow engine as a library.

Reasons:
- Conductor's domain is code/repo orchestration. This is *personal communication obligations* — a different audience and mental model.
- The integrations needed (Slack API, Gmail, GitHub PR comments, Google Meet transcripts) would bloat conductor with unrelated dependencies.
- This tool may eventually run on mobile or as a browser extension — conductor is a CLI/TUI.

The workflow engine is the right primitive: trigger (capture request) → steps (remind, draft response, send) → done.

---

## Extracting the Workflow Engine as a Reusable Crate

### Proposed: `conductor-workflow` crate

Move the engine, step runner, and context/variable system out of `conductor-core` into a standalone crate published to crates.io.

**Dependency graph after extraction:**
```
conductor-workflow   (new, standalone)
       ↑
conductor-core       (depends on conductor-workflow)
       ↑
conductor-cli / conductor-tui / conductor-web

comms-tracker        (new tool, also depends on conductor-workflow)
```

### What the crate owns
- Workflow definition parsing (YAML)
- Step execution engine
- Context/variable propagation
- Retry and error handling logic

### What the host app provides (via traits)
- Storage backend (SQLite, postgres, in-memory)
- Agent/subprocess execution
- Secrets and credential access

### Effort
Medium — approximately one to two weeks to extract cleanly, define trait abstractions, and publish. The main design work is the interface between what the engine owns vs. what the host provides.

---

## Cross-Language Considerations

Making the engine available in other languages (Python, JS via WASM) is possible but significantly harder:

- The engine is sync Rust tied to SQLite + subprocess execution
- WASM would require async rewrites and a different storage backend
- Not worth pursuing unless a specific target language ecosystem is a hard requirement

**Recommendation:** defer until `conductor-workflow` is stable as a Rust crate.

---

## Rough Workflow Example (YAML)

What a Slack-originated request might look like in workflow terms:

```yaml
name: respond-to-slack-request
trigger:
  type: manual
  inputs:
    - name: requester
    - name: channel_id
    - name: message
    - name: deadline

steps:
  - id: capture
    type: record
    description: "Log the obligation to the tracker DB"

  - id: draft
    type: agent
    description: "Draft a response or artifact based on the request"
    prompt: "{{message}}"

  - id: remind
    type: wait_until
    until: "{{deadline - 1h}}"

  - id: send
    type: integration
    channel: slack
    target: "{{channel_id}}"
    body: "{{draft.output}}"
```

---

## Open Questions

1. How is a request captured? (CLI command, hotkey, browser extension, Slack slash command?)
2. How are channel credentials managed? (per-user config, OS keychain?)
3. Should the tracker have a TUI similar to conductor's, or is a simple CLI + notifications enough?
4. What's the right name for the standalone tool?
5. Should `conductor-workflow` extraction happen first, or can the new tool copy/vendor the engine initially and extract later?

---

## Next Steps

- [ ] Decide on capture mechanism (how requests enter the system)
- [ ] Design the `conductor-workflow` crate interface (traits for storage, execution)
- [ ] Prototype the Slack integration (send a message to a channel via bot token)
- [ ] Name the tool

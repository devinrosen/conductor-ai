# RFC 005: Codebase Diagram Workflows

**Status:** Implemented
**Created:** 2026-03-13

---

## Background

Understanding a codebase — how users move through it, how components connect, how data flows — is time-consuming and usually undocumented. Diagrams exist when someone takes the time to draw them, go stale immediately, and are rarely updated when features ship.

This RFC proposes a set of conductor workflows that extract, maintain, and analyze flowcharts for any registered repo. The goal is to make living diagrams a zero-effort byproduct of normal development, and to use those diagrams as a foundation for UX analysis and feature design.

---

## Design principles

**1. Diagrams live in the repo.**
All outputs are committed under `docs/diagrams/` so they are versioned alongside the code, visible on GitHub, and diff-able in PRs.

**2. Mermaid as the canonical format.**
Text-based, git-diffable, renders natively in GitHub markdown and PRs. No external tooling required.

**3. Workflows open PRs — humans iterate.**
Automated workflows write a first draft and open a PR. The product and engineering team refine from there using prompting and further workflow runs. The PR is the collaboration surface.

**4. Personas are team-defined, not inferred on every run.**
On first extraction, personas are proposed from the codebase and saved to `docs/diagrams/personas.md`. Subsequent runs use that file. The team owns and refines their persona definitions.

**5. Staleness should be invisible.**
Developers should never have to remember to update diagrams. Automation handles it. Manual invocation is always available as an escape hatch.

---

## Repository layout

```
docs/diagrams/
├── personas.md                  # Team-defined personas (generated on first run, then team-owned)
├── ux-flow.mmd                  # User journey flows
├── system-architecture.mmd      # Major components and connections
├── data-flow.mmd                # How data moves through the system
├── state-machines.mmd           # Key stateful flows (auth, onboarding, etc.)
├── api-integrations.mmd         # External services and internal API boundaries
├── database-schema.mmd          # Entity relationships
└── analysis/
    └── ux-analysis-<date>.md    # UX analysis reports (dated, append-only history)
```

---

## Workflows

### 1. `generate-diagrams` — Extract diagrams from a repo

Reads the codebase and generates the full set of diagrams from scratch.

**Inputs:**
- `--repo` — target repo slug (required)
- `--types` — comma-separated subset of diagram types to generate (optional; omit for all six)

**Behavior:**
- Generates all requested diagram types and writes them to `docs/diagrams/`
- On first run (or if `docs/diagrams/personas.md` does not exist): proposes personas extracted from the codebase (auth roles, user models, permission levels, onboarding flows) and writes them to `docs/diagrams/personas.md` for team review
- Opens a PR with all generated/updated files

**Diagram types:**
| Key | File | Description |
|-----|------|-------------|
| `ux` | `ux-flow.mmd` | User journeys through the product |
| `architecture` | `system-architecture.mmd` | Major components and how they connect |
| `data-flow` | `data-flow.mmd` | How data moves through the system |
| `state-machines` | `state-machines.mmd` | Key stateful flows — only generated if relevant ones are detected |
| `api` | `api-integrations.mmd` | External services and internal API boundaries |
| `db` | `database-schema.mmd` | Entity relationships |

**Example invocations:**
```
conductor workflow run generate-diagrams --repo my-app
conductor workflow run generate-diagrams --repo my-app --types ux,architecture
```

---

### 2. `update-diagrams` — Update diagrams based on a feature request

Given a feature ticket, updates the relevant diagrams to reflect the proposed change and opens a PR for review.

**Inputs:**
- `--repo` — target repo slug (required)
- `--ticket` — conductor ticket ID (primary input)
- `--types` — limit to specific diagram types (optional; default: all relevant ones)

**Behavior:**
- Reads the ticket title and body from conductor's DB
- Extracts any Figma links from the ticket body and uses them as additional UX context
- If the ticket is under-specified (missing acceptance criteria, ambiguous scope, unclear user impact): outputs a list of open questions and stops without modifying any diagrams
- Otherwise: identifies which diagrams are affected by the feature and updates them
- Uses Option C staleness handling: generates proposed changes, shows a diff, asks for confirmation before writing
- Opens a PR with updated diagrams targeting the feature's worktree branch (if one exists) or `main`

**Figma integration:**
Figma links are extracted automatically from the ticket body (`figma.com/...`). If present, the workflow uses the linked design as the authoritative source for UX flow and persona interactions in the updated diagrams.

**Open question flagging:**
If the ticket does not have enough information to confidently update diagrams, the workflow outputs structured questions rather than guessing. Example output:
```
⚠ Ticket #42 is not refined enough to update diagrams. Open questions:

1. Which user personas are affected by this feature?
2. Does this change the authentication flow or is it additive?
3. What happens when the user cancels mid-flow?

Please refine the ticket and re-run.
```

**Example invocations:**
```
conductor workflow run update-diagrams --repo my-app --ticket TICKET-42
conductor workflow run update-diagrams --repo my-app --ticket TICKET-42 --types ux,state-machines
```

---

### 3. `analyze-ux` — Analyze diagrams from a user perspective

Reads the existing diagrams and `personas.md`, then produces a structured UX analysis report covering each persona's experience, friction points, and conflicts between competing use cases.

**Inputs:**
- `--repo` — target repo slug (required)
- `--personas` — comma-separated subset of personas to analyze (optional; default: all defined in `personas.md`)
- `--focus` — specific diagram type or flow to focus on (optional)

**Behavior:**
- Reads all diagrams from `docs/diagrams/` and personas from `docs/diagrams/personas.md`
- Analyzes each persona's journey through the relevant flows
- Identifies: friction points, dead ends, flows that work well for one persona but poorly for another, missing paths
- Writes a dated report to `docs/diagrams/analysis/ux-analysis-<date>.md`
- Opens a PR with the new report

**Report structure:**
```markdown
# UX Analysis — <date>

## Executive summary
...

## Per-persona analysis
### Admin
...
### End User
...

## Conflicts between personas
...

## Recommendations
...
```

**Example invocations:**
```
conductor workflow run analyze-ux --repo my-app
conductor workflow run analyze-ux --repo my-app --personas "Admin,End User"
conductor workflow run analyze-ux --repo my-app --focus ux
```

---

## Personas file format

`docs/diagrams/personas.md` is generated on first run and then owned by the team. Format:

```markdown
# Personas

## Admin
Manages users, configures the system, has full access to all features.
Primary goals: oversight, control, audit.

## End User
Day-to-day user of the product. May be technical or non-technical.
Primary goals: completing tasks quickly, minimal friction.

## API Consumer
Integrates with the product programmatically. Never uses the UI.
Primary goals: reliability, predictability, good error messages.
```

The workflow uses this file verbatim — the team's descriptions directly shape how the agent reasons about each persona.

---

## Staleness automation (follow-on, out of scope for v1)

A post-commit/merge hook could detect which source files changed, map them to relevant diagram types, and queue a background `generate-diagrams` run automatically. The developer sees updated diagrams appear in their next PR without ever invoking a workflow manually.

This is intentionally deferred. The mapping from source changes to diagram types is non-trivial and warrants its own design work. The three workflows above provide the manual foundation that the automation would build on.

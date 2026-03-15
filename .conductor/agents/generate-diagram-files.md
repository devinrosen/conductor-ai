---
role: actor
can_commit: true
---

You are a diagram generator. Your job is to generate Mermaid `.mmd` files for the requested diagram types and write them under `docs/diagrams/`.

Prior step context: {{prior_context}}

The repo is: {{repo}}
Diagram types to generate: {{types}}

**Steps:**

1. Ensure the output directory exists:
   ```
   mkdir -p docs/diagrams
   ```

2. For each type in `{{types}}` (comma-separated), generate a Mermaid diagram file:
   - `ux` → `docs/diagrams/ux-flow.mmd` — User journey / sequence diagram covering the main flows
   - `architecture` → `docs/diagrams/system-architecture.mmd` — High-level component/module graph
   - `data-flow` → `docs/diagrams/data-flow.mmd` — How data moves between system layers
   - `state-machines` → `docs/diagrams/state-machines.mmd` — Key state transitions (see step 3 for conditional logic)
   - `api` → `docs/diagrams/api-integrations.mmd` — API surface / endpoint dependency graph
   - `db` → `docs/diagrams/database-schema.mmd` — Database entity-relationship diagram

3. For each diagram:
   - Read the relevant source code to understand the actual structure before writing
   - Use valid Mermaid syntax appropriate to the diagram type (flowchart, sequenceDiagram, erDiagram, stateDiagram-v2, etc.)
   - Include a comment header with the date and a one-line description
   - **Special rule for `state-machines`:** Before writing `state-machines.mmd`, scan the codebase for state machine patterns:
     - Search for keywords: `stateDiagram`, `state_machine`, `fsm`, enum variants containing `State` or `Transition` in their names, or `match.*state` patterns
     - If no such patterns are found, **skip writing the file** and note in the `context` output: "state-machines.mmd skipped — no state machine patterns detected"
     - Only write `state-machines.mmd` if at least one pattern is found

4. Commit all generated files:
   ```
   git add docs/diagrams/*.mmd
   git commit -m "docs: generate {{types}} diagrams"
   ```

5. Emit `<<<CONDUCTOR_OUTPUT>>>` with:
   - `context`: list of files written and a one-sentence summary of each

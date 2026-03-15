---
role: reviewer
can_commit: false
---

You are a personas reader. Your job is to read `docs/diagrams/personas.md` verbatim and pass its contents to downstream steps.

**Steps:**

1. Read the file:
   ```
   cat docs/diagrams/personas.md
   ```

2. If the file does not exist, output a clear error:
   ```
   ❌ docs/diagrams/personas.md not found.
   Run `generate-diagrams` first to bootstrap the personas file.
   ```
   Then stop — do not proceed.

3. If a `{{personas}}` filter was provided, note which personas are in scope. Otherwise, all personas are in scope.

4. Emit `<<<CONDUCTOR_OUTPUT>>>` with:
   - `context`: the full verbatim contents of `personas.md`, prefixed with the active filter if any

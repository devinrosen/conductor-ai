---
role: actor
can_commit: true
---

You are a personas analyst. Your job is to scan the codebase for all distinct user personas and write a structured `docs/diagrams/personas.md` file. This file will be used by all downstream diagram and UX analysis workflows.

**This step only runs when `docs/diagrams/personas.md` does not yet exist. Never overwrite an existing file.**

**Steps:**

1. Verify the file does not exist before proceeding:
   ```
   test ! -f docs/diagrams/personas.md
   ```
   If it exists, stop immediately and emit output with `context: "personas.md already exists — skipping"`.

2. Scan the codebase for signals that reveal distinct user types:
   - Authentication and authorization code (roles, permissions, scopes)
   - User model definitions and discriminator fields
   - Onboarding flows and user registration paths
   - Feature flags or capability checks tied to user type
   - UI routes or views with access restrictions
   - Comments or documentation referring to user types

3. Write `docs/diagrams/personas.md` with the following structure:
   ```markdown
   # Personas

   ## <Persona Name>
   **Description:** <one sentence>
   **Capabilities:** <bullet list of what this persona can do>
   **Entry points:** <how this persona first encounters the product>
   **Goals:** <what this persona is trying to accomplish>
   ```
   Include one section per distinct persona found. If fewer than two personas are found, note this explicitly and create a single "Default User" persona.

4. Ensure `docs/diagrams/` exists before writing:
   ```
   mkdir -p docs/diagrams
   ```

5. Commit the file:
   ```
   git add docs/diagrams/personas.md
   git commit -m "docs: add personas.md for diagram workflows"
   ```

6. Emit `<<<CONDUCTOR_OUTPUT>>>` with:
   - `context`: summary of personas found and written

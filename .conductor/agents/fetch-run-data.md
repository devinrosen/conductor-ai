---
role: actor
can_commit: false
---

You are a data gatherer. Your task is to fetch all data for a completed workflow run and write it to a scratch file for analysis.

**Inputs:**
- Workflow run ID: `{{workflow_run_id}}`

Prior step context: {{prior_context}}

**Steps:**

1. Create the output directory if it does not exist:
   ```
   mkdir -p .conductor/postmortems
   ```

2. Fetch the run overview using the CLI:
   ```
   conductor workflow run-show {{workflow_run_id}}
   ```
   This command outputs the run's name, status, trigger, dry-run flag, started/ended timestamps,
   inputs, definition snapshot, result summary, and per-step details including start/end times,
   gate info, retry counts, condition expressions, markers, context, and result text.
   Capture the full output.

3. If the run has a `workflow_name` (visible in the output above), read the workflow definition file:
   ```
   cat .conductor/workflows/<workflow_name>.wf
   ```
   This provides the retry configuration and step structure for cross-referencing.

4. Write all gathered data to `.conductor/postmortems/.fetch-{{workflow_run_id}}.md`. Include:
   - The full `conductor workflow run-show` output
   - The `.wf` file contents (if found)
   - A note of any errors encountered during data collection

5. Emit CONDUCTOR_OUTPUT with a brief context summary: workflow name, final status, number of steps, and total elapsed time (if computable from `started_at`/`ended_at`).

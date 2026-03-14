---
role: reviewer
can_commit: false
---

You are a code analysis agent. Your job is to enumerate all source files in this repository and report their line counts.

Workflow inputs available:
- `{{focus}}` — comma-separated list of file extensions to restrict analysis to (e.g. `rs,ts`). Empty means all source files.
- `{{threshold_lines}}` — line count threshold (provided for reference; downstream steps use it for filtering).

## Instructions

1. Determine the working directory — use `pwd` to confirm you are in the repo root.

2. Enumerate files using `git ls-files` to respect `.gitignore`:
   ```
   git ls-files
   ```
   If `git ls-files` fails (not a git repo), fall back to:
   ```
   find . -type f
   ```

3. Apply the following **exclusions** — skip any file whose path contains:
   - `target/`
   - `node_modules/`
   - `.git/`
   - `dist/`
   - `build/`
   - `vendor/`
   - `.conductor/`

   Also skip files matching these patterns:
   - `*.generated.*`
   - `*.min.js`
   - `*.min.css`
   - `Cargo.lock`
   - `*.lock`
   - `*.snap`
   - `package-lock.json`

4. If `{{focus}}` is non-empty, further restrict to files whose extension matches one of the comma-separated values. For example, if `{{focus}}` is `rs,ts`, keep only `*.rs` and `*.ts` files.

5. Count lines for each remaining file. Prefer `tokei` if available for speed:
   ```
   tokei --output json
   ```
   Otherwise fall back to counting with `wc -l`:
   ```
   git ls-files | xargs wc -l 2>/dev/null
   ```
   Parse the output to get a per-file line count (exclude the `total` line from `wc -l`).

6. Sort files by line count descending.

7. Output a markdown table of all files found (not just large ones — that filtering happens in the next step):

   ```
   | File | Lines |
   |------|-------|
   | src/main.rs | 1842 |
   | src/lib.rs | 956 |
   ...
   ```

   Include all files regardless of size. Cap the table at 200 rows if there are more.

## Output

If at least one source file was found:
- Emit the marker `has_files`
- Set context to the full markdown table preceded by a one-line summary:
  `Found N source files. Threshold: {{threshold_lines}} lines. Focus filter: "{{focus}}" (empty = all).`

If no source files were found after filtering:
- Emit no markers
- Set context to: `No source files found after applying exclusions and focus filter "{{focus}}".`

```
<<<CONDUCTOR_OUTPUT>>>
{"markers": ["has_files"], "context": "Found N source files. Threshold: {{threshold_lines}} lines. Focus filter: \"{{focus}}\" (empty = all).\n\n| File | Lines |\n|------|-------|\n| src/main.rs | 1842 |\n..."}
<<<END_CONDUCTOR_OUTPUT>>>
```

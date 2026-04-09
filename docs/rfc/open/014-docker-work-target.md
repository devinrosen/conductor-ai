# RFC 014: Docker Work Target

**Status:** Draft
**Date:** 2026-04-08
**Author:** Devin

---

## Problem

Conductor's worktree model provides file-system isolation per ticket — each worktree gets its own branch and directory. This works well for pure-code projects, but breaks down for projects that require backing services (databases, caches, queues) to do meaningful work.

Today, a developer working across three tickets simultaneously shares a single local Postgres instance. Running migrations or seeding test data on one ticket's branch pollutes the others. The workaround — manual schema snapshots, careful fixture management, or serial work — undermines the parallel-agent value that Conductor provides.

---

## Goals

- Let developers declare a Docker Compose environment alongside their code, and have Conductor manage its lifecycle per worktree.
- Keep the agent's experience unchanged: it runs in a tmux window on the host, in the worktree directory, exactly as it does today.
- Zero impact on existing worktrees — additive and opt-in only.
- Design the abstraction so that future target types (e.g., agent-in-container) can be added without breaking the current interface.

## Non-Goals

- **Agent-in-container (Model B):** Running the `claude` CLI inside the Docker container is explicitly out of scope for v1. It requires `claude` to be installed in the project's image, OAuth tokens to be bind-mounted in (a credential exposure risk), and tight coupling between Conductor's auth and arbitrary container images. The trait is designed to accommodate this later.
- **Kubernetes targets:** A different complexity tier. Not this RFC.
- **Docker image building:** Conductor does not build images. The project's existing `docker-compose.dev.yml` (or equivalent) is assumed to work standalone.
- **Secret management:** Conductor does not store or inject secrets. Secrets flow through the project's existing `.env` file at repo root, read by Compose as it does today.

---

## Proposed Design

### Mental Model

A **work target** is the environment in which a worktree's agent operates. Today there is one target type: `git` (the worktree directory on the host). This RFC adds a second: `docker` (a Compose stack running alongside the worktree, with ports allocated per worktree to prevent conflicts).

```
Worktree (code + branch)
  └── WorkTarget
        ├── GitTarget (default, today)       — no-op, agent uses worktree dir
        └── DockerTarget (new)               — provisions a Compose stack per worktree
                                               injects resolved env into agent session
```

The agent always runs on the host in a tmux window. The Docker target changes what environment variables the agent session receives, and manages the Compose stack lifecycle.

---

### Configuration

Docker target configuration lives in `.conductor/docker.toml` in the repository root, committed to version control. This follows the existing pattern of `.conductor/*.wf` workflow files.

```toml
# .conductor/docker.toml

[work_target]
type = "docker"

# Path to the Compose file, relative to repo root.
# Defaults to "docker-compose.dev.yml" if omitted.
compose_file = "docker-compose.dev.yml"

# The primary service name — used to determine which service's
# mapped ports to resolve for env injection.
primary_service = "app"

# Port range reserved for this repo's worktrees.
# Conductor allocates blocks within this range, one per worktree.
# The range must be large enough for (max_concurrent_worktrees × ports_per_worktree).
# Defaults to 15000–19999 if omitted.
port_range = [15000, 19999]

# Environment variables to inject into the agent's tmux session.
# Values may reference {PORT:<service>:<container_port>} placeholders,
# which Conductor resolves to the allocated host port for that mapping.
[work_target.env]
DATABASE_URL = "postgresql://postgres:password@localhost:{PORT:postgres:5432}/app_dev"
REDIS_URL = "redis://localhost:{PORT:redis:6379}"
```

No changes to `~/.conductor/config.toml`. This is repo-level configuration, shared via git.

---

### Port Isolation

The core problem: two worktrees cannot both bind `5432` on the host. Conductor solves this by owning port allocation per worktree.

**Allocation strategy:**

1. On `provision`, Conductor queries the DB for all ports allocated within the repo's configured range.
2. It finds a contiguous free block large enough for all mapped ports in the Compose file (by parsing the `ports:` entries).
3. It records the allocation in the DB (worktree → `{service}:{container_port}` → `host_port`).
4. It writes a `.conductor-ports.env` file into the worktree directory with the resolved bindings:
   ```
   CONDUCTOR_PORT_POSTGRES_5432=15432
   CONDUCTOR_PORT_REDIS_6379=15433
   ```
5. It runs `docker compose --env-file .conductor-ports.env up -d` using the Compose project name `conductor-<repo_slug>-<worktree_slug>` for full network isolation.
6. On `teardown`, it runs `docker compose down` and releases the port allocation in the DB.

The `.conductor-ports.env` file is in `.gitignore` (Conductor adds this on first provision). It is worktree-local and ephemeral.

**The Compose file must use variable references for port bindings:**

```yaml
# docker-compose.dev.yml
services:
  postgres:
    image: postgres:16
    ports:
      - "${CONDUCTOR_PORT_POSTGRES_5432:-5432}:5432"  # falls back to 5432 without Conductor
    environment:
      POSTGRES_PASSWORD: password

  redis:
    image: redis:7
    ports:
      - "${CONDUCTOR_PORT_REDIS_6379:-6379}:6379"
```

The fallback (`:-5432`) means the Compose file works standalone (without Conductor) for developers who don't use the Docker target.

---

### Trait Surface

```rust
/// The environment in which a worktree's agent operates.
pub trait WorkTarget {
    /// Provision the target environment.
    /// For DockerTarget: allocates ports, writes .conductor-ports.env, runs compose up.
    /// For GitTarget: no-op.
    fn provision(&self) -> Result<ProvisionResult>;

    /// The working directory for the agent process.
    /// Always the worktree path for both target types in v1.
    fn agent_cwd(&self) -> &Path;

    /// Extra environment variables to inject into the agent's tmux session.
    /// For DockerTarget: resolved DATABASE_URL, REDIS_URL, etc. from .conductor/docker.toml.
    /// For GitTarget: empty.
    fn agent_env(&self) -> HashMap<String, String>;

    /// Check whether the target environment is alive.
    /// Used by the orphan reaper and TUI status.
    fn health(&self) -> TargetHealth;

    /// Tear down the target environment and release resources.
    /// For DockerTarget: compose down + release port allocation in DB.
    /// For GitTarget: no-op.
    fn teardown(&self) -> Result<()>;
}

pub enum TargetHealth {
    /// Environment is up and healthy.
    Healthy,
    /// Environment exists but something is wrong (e.g. a container exited).
    Degraded { reason: String },
    /// Environment is gone (e.g. Compose stack not running).
    Gone,
}

pub struct ProvisionResult {
    /// Human-readable summary of what was provisioned (shown in TUI/CLI on worktree create).
    pub summary: String,
}
```

---

### Lifecycle

```
worktree create
  └── if .conductor/docker.toml exists and type = "docker"
        → DockerTarget::provision()
          1. Parse compose file, identify port mappings
          2. Allocate host ports from repo's range (DB)
          3. Write .conductor-ports.env to worktree dir
          4. docker compose --env-file .conductor-ports.env \
               --project-name conductor-<repo>-<wt> up -d
          5. Record target_type + port allocation in worktrees table

agent start (existing flow, no change)
  └── AgentManager reads worktree.work_target_type
        → if "docker": resolve env vars, inject into tmux session env
        → if "git": no change

worktree delete
  └── DockerTarget::teardown()
        1. docker compose --project-name conductor-<repo>-<wt> down
        2. Release port allocation from DB
        3. Delete .conductor-ports.env
```

**Failure handling:**

- If `compose up` fails during `worktree create`, the worktree is still created (branch + directory exist) but `work_target_state` is set to `{ "status": "degraded", "error": "..." }`. The user sees a warning, not a hard failure. They can manually fix the Compose file and re-provision.
- A `conductor worktree reprovision <name>` command handles the re-provision case.

---

### DB Changes

New migration (`063_docker_work_target.sql`):

```sql
ALTER TABLE worktrees ADD COLUMN work_target_type TEXT NOT NULL DEFAULT 'git';
ALTER TABLE worktrees ADD COLUMN work_target_state TEXT; -- JSON blob

CREATE TABLE IF NOT EXISTS worktree_port_allocations (
    id          TEXT PRIMARY KEY,
    worktree_id TEXT NOT NULL REFERENCES worktrees(id) ON DELETE CASCADE,
    repo_id     TEXT NOT NULL,
    service     TEXT NOT NULL,   -- e.g. "postgres"
    container_port INTEGER NOT NULL, -- e.g. 5432
    host_port   INTEGER NOT NULL,
    created_at  TEXT NOT NULL,
    UNIQUE(repo_id, host_port)   -- prevent double-allocation within a repo's range
);
```

`work_target_state` JSON shape for Docker targets:

```json
{
  "status": "running" | "degraded" | "stopped",
  "compose_project": "conductor-myrepo-my-ticket",
  "error": null
}
```

---

### Orphan Reaper Extension

The existing `AgentManager::reap_orphaned_runs()` checks tmux window liveness. Docker targets add a second health axis: the Compose stack can die without killing the tmux window (the agent would see connection errors instead of a clean exit).

`DockerTarget::health()` runs `docker compose --project-name <name> ps --format json` and returns:
- `Healthy` if all services are `running`
- `Degraded` if any service has exited
- `Gone` if the project is not found

The background poll (TUI: `background.rs` tick; Web: periodic tokio task) calls `health()` on each active Docker-targeted worktree and updates `work_target_state` in the DB. The TUI renders a container status indicator alongside the existing agent status.

---

### TUI / CLI Surface

**TUI worktree list:** Docker-targeted worktrees show a container status badge next to the agent status badge. States: `[container: up]`, `[container: degraded]`, `[container: stopped]`.

**CLI:**
```
conductor worktree create <repo> <name>          # unchanged; auto-provisions if .conductor/docker.toml exists
conductor worktree reprovision <repo> <name>     # new: re-run provision (fix degraded target)
conductor worktree delete <repo> <name>          # unchanged; auto-teardown
conductor worktree status <repo> <name>          # shows target type + container health
```

---

## Open Questions

1. **Compose file validation on `worktree create`:** Should Conductor validate that the compose file parses and all referenced port variables are present before running `compose up`? Early validation gives better error messages; lazy validation is simpler.

2. **Port range conflicts across repos:** The default port range (`15000–19999`) is shared across all repos using the Docker target. If two different repos both use the default, their worktrees draw from the same pool. Should the range be global (one pool) or per-repo (enforced by the config)? Global pool is simpler; per-repo avoids cross-repo confusion.

3. **Re-provision on agent start:** Should Conductor automatically attempt `docker compose up -d` if `work_target_state.status != "running"` when an agent is started? This would self-heal degraded targets. Risk: masking real failures; benefit: smoother recovery.

4. **Image pulling on provision:** `compose up` will pull images if not cached, which can be slow. Should Conductor surface this in the TUI progress modal? The current blocking compose call needs to run off-thread per the TUI threading rule.

---

## Implementation Plan

1. **Trait + GitTarget** — extract `WorkTarget` trait, implement `GitTarget` as a no-op. All existing behavior preserved. No DB changes yet.
2. **DB migration** — add `work_target_type`, `work_target_state`, and `worktree_port_allocations`.
3. **Port allocator** — `PortAllocator` struct that reads the Compose file, finds free ports in the configured range, writes `.conductor-ports.env`.
4. **DockerTarget** — implement `provision`, `health`, `teardown`. Wire into `WorktreeManager::create` and `WorktreeManager::delete`.
5. **Agent env injection** — `AgentManager` reads `work_target_type` and calls `agent_env()` before launching the tmux session.
6. **Orphan reaper extension** — add Docker health check to background poll.
7. **TUI badge** — container status indicator in worktree list.
8. **CLI `reprovision` command** — wraps `DockerTarget::provision()` for manual recovery.

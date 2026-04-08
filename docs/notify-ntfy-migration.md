# Migrating from Web Push to ntfy.sh

Conductor's built-in Web Push notifications are being removed in a follow-on
change (PR #1891). This guide walks you through switching to
[ntfy.sh](https://ntfy.sh) — a lightweight, open-source pub/sub push service
with native iOS/Android apps, browser support, and a trivial HTTP API.

> **You don't need to migrate immediately.** Web Push continues to work until
> PR #1891 lands. Use this guide to set up ntfy before that change ships.

---

## Why ntfy.sh?

| | Web Push | ntfy.sh |
|---|---|---|
| iOS / Android app | via browser | native app (free) |
| Self-hosting | no | yes (Docker, single binary) |
| Setup complexity | OAuth flow + server-side keys | pick a topic name, done |
| Requires a browser | yes | no |
| Custom priority / tags | no | yes |
| Free tier | yes | yes (public server) |
| Private notifications | no (shared infra) | yes (self-host or token auth) |

---

## One-Time Setup

1. **Install the ntfy app** on your phone from the
   [App Store](https://apps.apple.com/app/ntfy/id1625396347) or
   [Google Play](https://play.google.com/store/apps/details?id=io.heckel.ntfy).
   On desktop, the web app at [ntfy.sh](https://ntfy.sh) works too.

2. **Pick an unguessable topic name.** The topic is public by default — anyone
   who knows the name can publish to it. Use a random string, e.g.:

   ```
   conductor-a7f3k9mq2x
   ```

   See the [Security](#security) section below before choosing a topic name.

3. **Subscribe** to your topic in the ntfy app by tapping **+** and entering
   your topic name.

---

## Quickstart: inline one-liner

The simplest approach needs no script file at all — just a `curl` command
inline in `config.toml`:

```toml
[[notify.hooks]]
on  = "*"
run = "curl -s -d \"$CONDUCTOR_LABEL\" -H \"Title: Conductor — $CONDUCTOR_EVENT\" https://ntfy.sh/your-topic-here"
```

Replace `your-topic-here` with the topic name you chose above.

---

## Full Hook Setup

For event-aware priorities, emoji tags, and optional auth, use the richer hook
script shipped with Conductor.

### Shell script (`notify-ntfy.sh`)

1. Copy the script and make it executable:

   ```bash
   mkdir -p ~/.conductor/hooks
   cp /path/to/conductor/docs/examples/hooks/notify-ntfy.sh ~/.conductor/hooks/
   chmod +x ~/.conductor/hooks/notify-ntfy.sh
   ```

2. Set your topic (and optionally server/token) in your shell profile or
   directly in `config.toml` via `env`:

   ```bash
   export NTFY_TOPIC="conductor-a7f3k9mq2x"
   # export NTFY_SERVER="https://ntfy.your-domain.com"  # if self-hosting
   # export NTFY_TOKEN="your-auth-token"                # if using token auth
   ```

3. Wire it up in `~/.conductor/config.toml`:

   ```toml
   [notifications]
   enabled = true

   [[notify.hooks]]
   on  = "*"
   run = "~/.conductor/hooks/notify-ntfy.sh"
   ```

4. Test it:

   ```bash
   conductor notifications test workflow_run.completed
   ```

### Python alternative (`notify-ntfy.py`)

If you prefer Python (no pip dependencies required):

```bash
cp /path/to/conductor/docs/examples/hooks/notify-ntfy.py ~/.conductor/hooks/
chmod +x ~/.conductor/hooks/notify-ntfy.py
```

Then update `config.toml` to point at `notify-ntfy.py` instead.

---

## Priority Mapping

The richer hook scripts map Conductor events to ntfy priorities automatically:

| Event pattern | ntfy priority | Effect |
|---|---|---|
| `*.failed` | `urgent` | Full volume, bypasses Do Not Disturb |
| `gate.waiting`, `gate.pending_too_long`, `feedback.requested` | `high` | Elevated alert |
| `*.cost_spike`, `*.duration_spike` | `high` | Elevated alert |
| `*.completed` | `default` | Standard notification |
| anything else | `default` | Standard notification |

---

## Self-Hosting ntfy

To avoid relying on the public ntfy.sh server, run your own:

```bash
docker run -p 80:80 -v /var/cache/ntfy:/var/cache/ntfy binwiederhier/ntfy serve
```

Then set `NTFY_SERVER=http://your-host` before running the hook.

See the [ntfy self-hosting docs](https://docs.ntfy.sh/install/) for TLS,
authentication, and persistent storage options.

---

## Security

**Public ntfy topics are a shared namespace.** If you pick a common topic name
like `conductor-notifications`, anyone else using ntfy.sh can publish messages
to your topic — and subscribe to receive yours.

Protect yourself by doing one of the following:

- **Use a long, random topic name** (e.g. a UUID or a random alphanumeric
  string of 16+ characters). Unguessable topics are effectively private.
- **Enable access control on a self-hosted server** — ntfy supports token auth
  and per-topic ACLs.
- **Use `NTFY_TOKEN`** with a private ntfy.sh account to restrict who can
  publish to your topics (requires a ntfy Pro account or self-hosting).

---

## Further Reading

- [docs/examples/hooks/README.md](examples/hooks/README.md) — full list of
  `CONDUCTOR_*` env vars and hook setup instructions
- [ntfy.sh documentation](https://docs.ntfy.sh)
- [ntfy self-hosting guide](https://docs.ntfy.sh/install/)

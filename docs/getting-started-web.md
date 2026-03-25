# Getting Started with Conductor Web

This guide covers running the `conductor-web` UI — a browser-based interface for managing repos, worktrees, tickets, and workflow runs.

## Prerequisites

- Everything listed in [Getting Started with the CLI](getting-started-cli.md)
- **[Bun](https://bun.sh/)** — for building the frontend

## Build & Run

```bash
# Build the React frontend (must be done first)
cd conductor-web/frontend && bun install && bun run build && cd ../..

# Build and run the web server
cargo run --bin conductor-web
```

The server starts at **http://localhost:3000**.

## Remote Access via Tailscale

If you want to access the web UI from another device on your [Tailscale](https://tailscale.com/) network, use `tailscale serve` to proxy traffic to the local server:

```bash
tailscale serve --bg 3000
```

This forwards your Tailscale hostname to `localhost:3000` with automatic HTTPS.

> **macOS note:** The Tailscale CLI isn't on your PATH by default. Either use the full path or create an alias:
> ```bash
> # Full path
> /Applications/Tailscale.app/Contents/MacOS/Tailscale serve --bg 3000
>
> # Or add an alias to ~/.zshrc
> alias tailscale="/Applications/Tailscale.app/Contents/MacOS/Tailscale"
> ```

To stop serving:

```bash
tailscale serve --bg 3000 off
```

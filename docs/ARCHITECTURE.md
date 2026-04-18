# Themis — Architecture

The technical shape of Themis, locked. Companion to [VISION.md](VISION.md).

## Overview

```
                ┌───────────────────────────┐
                │        Themisfile         │
                │ (declarative fabric spec) │
                └────────────┬──────────────┘
                             │
                             ▼
┌──────────────┐    ┌────────────────────┐
│    themis    │───▶│                    │
│    (CLI)     │    │                    │
└──────────────┘    │      themisd       │    ┌─────────────┐
                    │  (userspace daemon)│───▶│   SQLite    │
┌──────────────┐    │                    │    │   (state)   │
│  themis-tui  │───▶│                    │    └─────────────┘
│    (TUI)     │    └──────────┬─────────┘
└──────────────┘               │
                               ▼
                    ┌────────────────────┐
                    │   runtime layer    │
                    │ SSH · libvirt ·    │
                    │ host commands      │
                    └──────────┬─────────┘
                               │
                               ▼
                    ┌────────────────────┐
                    │      Host OS       │
                    │ KVM · libvirt ·    │
                    │ QEMU · Linux       │
                    └────────────────────┘
```

Clients speak gRPC to `themisd` over a unix socket. The daemon owns all state and persists it to SQLite. The runtime layer executes actions against libvirt and guest VMs. All three binaries share types via the `themis-proto` and `themis-core` crates.

## Binaries

**`themisd`** — standalone userspace daemon.
- Owns all lab state; single writer.
- gRPC server on `$XDG_RUNTIME_DIR/themisd.sock`.
- Lifecycle: explicit start/stop, or lazy-start by clients.
- Reconciles running labs against libvirt on restart.

**`themis`** — CLI client.
- `clap`-based argument parsing.
- Speaks gRPC to `themisd`. Same capability surface as the TUI.
- Auto-starts `themisd` if the socket is absent.

**`themis-tui`** — terminal UI client.
- `ratatui` + `crossterm`. Speaks gRPC to `themisd`.
- Subscribes to state streams for live updates.
- Surfaces: topology canvas, node inspector, chaos driver, event feed.

## Workspace layout

Cargo workspace at the repo root. Nine crates:

| Crate | Purpose |
|---|---|
| `themis-proto` | `.proto` files + generated tonic/prost types |
| `themis-core` | Domain types, errors, extension traits |
| `themis-compiler` | Themisfile → topology → configs + seeds |
| `themis-runtime` | SSH (`russh`), libvirt (shelled `virsh`), host commands |
| `themis-templates` | Built-in topology implementations |
| `themis-platforms` | Built-in NOS platform implementations |
| `themisd` | Daemon binary |
| `themis` | CLI binary |
| `themis-tui` | TUI binary |

Async runtime: `tokio` throughout. Serialization: `serde`.

## The Themisfile

A single declarative file defining one fabric. Filename is `Themisfile`. Format is KDL.

```kdl
fabric "my-lab" {
    template "clos-3tier"
    platform "frr-fedora"
    wan-interface "eth0"

    parameters {
        borders 2
        spines 2
        racks 4
        servers-per-rack 4
    }
}
```

Parsed by the compiler via `kdl-rs` (Apache-2.0 OR MIT), validated against the named template's parameter schema.

## Data flow

```
Themisfile
    │
    ▼
┌───────────────────────────────┐
│     themis-compiler           │
│  ──────────────────────────   │
│  loader    — parse & validate │
│  expander  — template.expand  │
│  estimator — resource plan    │
│  inventory — node graph       │
│  renderer  — NOS configs      │
└───────────────┬───────────────┘
                │  topology + artifacts
                ▼
         themisd (orchestrator)
                │
                ▼
┌───────────────────────────────┐
│     themis-runtime            │
│  ──────────────────────────   │
│  host     — ip, bridge, iptables, cloud-localds
│  libvirt  — virsh (create, destroy, list)
│  ssh      — push configs, run chaos
└───────────────┬───────────────┘
                │  state updates
                ▼
            SQLite
```

Clients observe state via gRPC streaming subscriptions.

## Extension contracts

Two extension points, each one trait. No dynamic plugin loading — built-ins are registered at compile time.

**Topology template** (`themis-core::Template`):
```rust
trait Template {
    fn name(&self) -> &str;
    fn schema(&self) -> &ParameterSchema;
    fn expand(&self, params: &Parameters) -> Result<Topology>;
}
```

**NOS platform** (`themis-core::Platform`):
```rust
trait Platform {
    fn name(&self) -> &str;
    fn node_roles(&self) -> &[Role];
    fn generate_config(
        &self,
        node: &Node,
        topology: &Topology,
        env: &minijinja::Environment,
    ) -> Result<HashMap<PathBuf, String>>;
    fn push_config(
        &self,
        session: &SshSession,
        configs: &HashMap<PathBuf, String>,
    ) -> Result<()>;
}
```

Built-ins live in `themis-templates` and `themis-platforms`. New ones are added by implementing the trait and registering at compile time in the respective crate.

## Protocol

gRPC over unix socket. Schema lives in `themis-proto/themis.proto` and is the source of truth.

Service surface:
- **Lab lifecycle** — define, list, inspect, deploy, destroy.
- **Runtime control** — push-config, run-chaos, pause, resume.
- **State streams** — subscribe to lab events, node transitions, chaos events.
- **Daemon** — health, version, shutdown.

Client types are generated from `.proto`; domain types in `themis-core` convert to/from wire types.

## State

SQLite, single file at `$XDG_STATE_HOME/themisd/state.db`. Accessed through `rusqlite`.

Tables:
- `labs` — one row per defined lab, with status and Themisfile snapshot.
- `nodes` — per-node state (provisioning, running, failed, destroyed).
- `links` — fabric edges and bridge bindings.
- `events` — append-only log of state transitions, chaos events, errors.

On daemon restart: read `labs` and `nodes`, reconcile against `virsh list`, mark stale rows, emit recovery events.

## Lifecycle

**Daemon:**
- Start: explicit (`themisd`) or lazy (client auto-starts on absent socket).
- Stop: SIGTERM → flush SQLite, close socket, exit.
- Crashed daemon: user restarts; reconciliation handles recovery.

**Lab:**
```
defined → provisioning → running → destroying → destroyed
                     ↘ failed ↗
             paused  ⇄  running
```

## On-disk layout

| Path | Purpose |
|---|---|
| `$XDG_RUNTIME_DIR/themisd.sock` | gRPC socket |
| `$XDG_STATE_HOME/themisd/state.db` | SQLite state |
| `$XDG_STATE_HOME/themisd/logs/` | rotated daemon logs |
| `$XDG_DATA_HOME/themisd/golden-images/` | baked base images |
| `$XDG_CACHE_HOME/themisd/generated/<lab>/` | per-lab generated configs and seeds |

No files outside the user's XDG tree. No root required beyond the user's group membership for libvirt and KVM.

## License enforcement

`cargo-deny` in CI. `deny.toml`:

```toml
[licenses]
allow = ["MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause",
         "ISC", "0BSD", "CC0-1.0", "Unlicense"]
deny  = ["GPL-1.0", "GPL-2.0", "GPL-3.0", "LGPL-2.0", "LGPL-2.1",
         "LGPL-3.0", "AGPL-1.0", "AGPL-3.0"]
confidence-threshold = 0.9
```

Attribution file `THIRD_PARTY_LICENSES` generated from `Cargo.lock` by `cargo-about` at release time.

## Replacement of the current tree

The current Python generator and Ansible playbooks are retired. The golden-image bake script (`golden-bootstrap/bake.sh`) survives as a host-side utility, invoked by `themis-runtime`; its Fedora + FRR recipe is unchanged.

---

*Architecture locked. No changes past this document.*

# Themis — Work Plan

From locked architecture to released artifact. Sequenced phases with explicit parallelism boundaries. Companion to [VISION.md](VISION.md) and [ARCHITECTURE.md](ARCHITECTURE.md).

## Strategy

- **Phases 0–1** set the tree up clean.
- **Phases 2–6** are the migration — high parallelism; multiple agents can work concurrently once Phase 2 lands.
- **Phases 7–10** are the sequential core build — each is a larger chunk, best done focused, one at a time.
- **Phases 11–14** are polish, packaging, and release.

## Target tree

```
Themis/
├── Cargo.toml                  # workspace root
├── Cargo.lock
├── LICENSE-MIT
├── LICENSE-APACHE
├── README.md
├── THIRD_PARTY_LICENSES        # generated at release
├── deny.toml                   # cargo-deny policy
├── docs/
│   ├── VISION.md
│   ├── ARCHITECTURE.md
│   ├── WORK_PLAN.md
│   ├── USER_GUIDE.md
│   ├── CHAOS_DSL.md
│   └── INSTALLATION.md
├── crates/
│   ├── themis-proto/
│   ├── themis-core/
│   ├── themis-compiler/
│   ├── themis-runtime/
│   ├── themis-templates/
│   ├── themis-platforms/
│   ├── themisd/
│   ├── themis/
│   └── themis-tui/
├── demos/
│   ├── clos-3tier-<scenario>/
│   ├── three-tier-<scenario>/
│   └── hub-spoke-<scenario>/
├── golden-bootstrap/           # retained, host-side utility
│   └── bake.sh
└── .github/
    └── workflows/
        ├── ci.yml
        └── release.yml
```

---

## Phase 0 — Audit & Inventory
**Sequential. No code changes. COMPLETE (2026-04-17).**

Tree walked; audit output below is authoritative for Phase 1.

### Keep (do not touch)

- `docs/` — contains the three locked planning docs (VISION, ARCHITECTURE, WORK_PLAN).
- `.git/` — repository history.
- `LICENSE` → **rename** to `LICENSE-APACHE` in Phase 1 (content is Apache-2.0 already). Add `LICENSE-MIT` alongside.

### Keep, with modification

- `golden-bootstrap/` — `bake.sh` and `Vagrantfile` stay. Architecture explicitly keeps this as a host-side utility.
- `.gitignore` — rewrite in Phase 1: drop Python/Ansible entries, keep editor/IDE/Vagrant/image entries, add Rust entries (`target/`, etc.).
- `README.md` — rewrite in Phase 1g against the locked vision.

### Relocate before deletion (Phase 1 pre-step)

Move these out of `platforms/` before deleting the directory — `golden-bootstrap/bake.sh` invokes them at bake time:

- `platforms/frr-fedora/image-recipe/provision.sh` → `golden-bootstrap/recipes/frr-fedora/provision.sh`
- `platforms/cumulus-vx/image-recipe/provision.sh` → `golden-bootstrap/recipes/cumulus-vx/provision.sh`

Update `bake.sh` to look for recipes at the new path.

### Delete (Phase 1)

- `ansible/` — entire tree (playbooks, roles, tasks).
- `ansible-requirements.yml`.
- `cli/` — Python Click CLI.
- `generated/` — empty scaffolding directory (gitignored anyway).
- `generator/` — Python compiler pipeline.
- `platforms/` — after relocation step above.
- `project.yml.example` — old config format, replaced by Themisfile (KDL).
- `requirements.txt` — Python deps.
- `templates/` — Python template implementations (including `clos-5stage/` and `hub-spoke/` stubs; clos-5stage is dropped per vision, hub-spoke is being reimplemented in Rust).
- `tmp/` — stray hosts file, gitignored.

### Port as reference (read-only during Phases 5–6, 10; not carried forward)

These will be deleted along with their containing directories. Their contents are ported by re-implementing in Rust during the relevant phase, not by copying files.

- `templates/clos-3tier/expander.py` + `templates/clos-3tier/template.yml` → reference for `themis-templates::clos_3tier` (Phase 5a).
- `platforms/frr-fedora/nos-driver/driver.py` + `platforms/frr-fedora/nos-driver/templates/*.j2` → reference for `themis-platforms::frr_fedora` (Phase 6a).
- `platforms/cumulus-vx/nos-driver/driver.py` (templates dir is empty — only a `.gitkeep`) → reference for `themis-platforms::cumulus_vx` (Phase 6b).
- `ansible/roles/chaos/tasks/*.yml` (latency, link-down, link-flap, link-up, node-kill, packet-loss, rack-partition) → reference for chaos primitives in `themis-runtime` (Phase 4) and the chaos DSL built-ins (Phase 10d).

### Phase 1 action order (enforces safe deletion)

1. Relocate the two `provision.sh` files to `golden-bootstrap/recipes/`.
2. Update `golden-bootstrap/bake.sh` to point at the new recipe paths.
3. Verify bake.sh still works (or at least parses) against new paths.
4. Take reference snapshots (read + internalize) of the port-as-reference files.
5. Delete everything in the "Delete" list.
6. Scaffold the Rust workspace per Phase 1.

**Done when:** this audit output exists in the work plan. Done.

---

## Phase 1 — Repo Reset
**Parallelizable. Sub-tasks independent.**

1a. Delete everything in the "goes" list.
1b. Create `crates/`; scaffold nine crates (`cargo new --lib`/`--bin`), empty placeholder modules.
1c. Root `Cargo.toml` as workspace. `[workspace.dependencies]` for shared crates (tokio, serde, thiserror, etc.). `[profile.release]` with `lto = true`, `codegen-units = 1`, `strip = true`.
1d. Write `deny.toml` with licensing policy from ARCHITECTURE.md.
1e. Update `.gitignore` for Rust (`target/`, `Cargo.lock` for libraries only where relevant, etc.). Remove Python/Ansible entries.
1f. `.github/workflows/ci.yml` — `cargo build --workspace`, `cargo test --workspace`, `cargo deny check`, `cargo fmt --check`, `cargo clippy -- -D warnings`.
1g. Rewrite `README.md` against locked vision. Short. Points to docs/.
1h. Add `LICENSE-MIT` and `LICENSE-APACHE` at repo root.

**Done when:** `cargo build --workspace` compiles clean, CI green, tree matches target layout.

---

## Phase 2 — Core Foundation
**Parallelizable within. 2a and 2b independent.**

2a. `themis-core` — domain types (`Topology`, `Node`, `Link`, `Role`, `ParameterSchema`, `Parameters`, `Bootstrap`, `Interface`, `BgpNeighbor`). Define `Template` and `Platform` traits. Errors via `thiserror`.

2b. `themis-proto` — write `themis.proto`. Services: `LabService`, `RuntimeService`, `StreamService`, `DaemonService`. `tonic-build` in `build.rs` for codegen.

2c. Conversion impls between `themis-core` and `themis-proto` types. Sequential after 2a + 2b.

**Done when:** both crates compile independently; round-trip conversion tests pass.

---

## Phase 3 — Compiler
**Parallelizable within. 3a–3e can each be a separate agent.**

3a. `themis-compiler::loader` — Themisfile parser via `kdl-rs`. Validates against template schema.
3b. `themis-compiler::expander` — invokes a `Template` trait impl, returns `Topology`.
3c. `themis-compiler::estimator` — RAM/vCPU/KSM projection from a `Topology`.
3d. `themis-compiler::inventory` — final node/link graph with addressing, MACs, bridge names.
3e. `themis-compiler::renderer` — `minijinja`-driven config rendering, invokes `Platform` trait impls.

**Done when:** given a valid Themisfile, the compiler produces complete artifacts in-memory for a known template+platform pair. (Disk writes live in runtime.)

---

## Phase 4 — Runtime
**Parallelizable within.**

4a. `themis-runtime::host` — `ip`, `bridge`, `iptables`, `cloud-localds` wrappers via `std::process::Command`.
4b. `themis-runtime::libvirt` — `virsh` shell-outs: `create`, `destroy`, `list`, `define`, `undefine`. Domain XML writer.
4c. `themis-runtime::iso` — cloud-init seed ISO building (`genisoimage` subprocess or native crate).
4d. `themis-runtime::ssh` — `russh` wrapper, connection pooling, parallel-execution helpers on tokio.

**Done when:** each primitive independently testable against a live libvirt install; unit tests cover success and failure paths.

---

## Phase 5 — Templates
**Parallelizable across three agents.**

5a. `themis-templates::clos_3tier` — port from Python. Reference: `templates/clos-3tier/expander.py`.
5b. `themis-templates::three_tier` — new. Traditional core/distribution/access. Core redundancy, distribution pairs, access switches per pair.
5c. `themis-templates::hub_spoke` — new. Hub + N branches, per-branch subnet plan, optional branch redundancy.

**Done when:** all three implement `Template`; unit tests verify node/link counts match expected values across a parameter sweep.

---

## Phase 6 — Platforms
**Parallelizable across two agents.**

6a. `themis-platforms::frr_fedora` — port Jinja2 templates to `minijinja`. Implement `generate_config` and `push_config`. Reference: `platforms/frr-fedora/`.
6b. `themis-platforms::cumulus_vx` — same. Reference: `platforms/cumulus-vx/`.

**Done when:** both implement `Platform`; rendered configs diff-equivalent to old Python output for identical topologies.

---

## Phase 7 — Daemon
**Sequential. One agent. Large chunk.**

7a. gRPC server skeleton — tonic, unix socket listener, tower middleware (tracing, error mapping).
7b. SQLite state store — `rusqlite`, schema DDL, typed access layer, single-writer discipline.
7c. Lab lifecycle state machine — `defined → provisioning → running → destroying → destroyed`, plus `failed` and `paused`.
7d. Event streaming — tokio broadcast channels, subscriber management, gRPC streaming adapter.
7e. Reconciliation — on startup, diff SQLite state against `virsh list`, emit recovery events, mark stale rows.
7f. Graceful shutdown — SIGTERM handler, flush, drain streams, close socket.
7g. Readiness endpoint — supports lazy-start detection from clients.

**Done when:** `themisd` starts, accepts gRPC, creates/deploys/destroys labs end-to-end, survives clean restart with state intact.

---

## Phase 8 — CLI
**Sequential. One agent.**

8a. `clap` top-level argument structure, subcommand skeletons.
8b. gRPC client connection; lazy-start `themisd` if socket absent.
8c. Commands: `init`, `define`, `list`, `inspect`, `deploy`, `destroy`, `push-config`, `logs`, `estimate`, `chaos`.
8d. Output: pretty tables by default, `--json` for scripting.
8e. Shell completion generation (bash, zsh, fish).

**Done when:** every daemon capability has a CLI subcommand; `--json` validated against proto shapes; lazy-start verified on clean install.

---

## Phase 9 — TUI (visual statement)
**Sequential. One agent. The biggest chunk.**

9a. ratatui + crossterm main loop, input handling, key bindings.
9b. gRPC streaming subscriptions — tokio task pool, state cache, diffing reducer.
9c. Topology canvas widget — nodes, edges, live-state coloring, layout engine for clos / three-tier / hub-spoke.
9d. Inspector pane — node drill-down, live BGP/interface/route tables.
9e. Event feed — filterable, timestamped, color-coded.
9f. Chaos surface — interactive scenario composition, live feedback rendering.
9g. Polish — Braille density, animation primitives, palette, keyboard + mouse.

**Done when:** TUI drives every daemon capability interactively, topology renders and updates live, runs smoothly on a 40-node lab.

---

## Phase 10 — Chaos DSL
**Sequential. One agent. Can run parallel to Phase 9 if agents are available.**

10a. Grammar — small declarative DSL for composable scenarios.
10b. Parser — `nom` or hand-rolled.
10c. Runtime — invokes `themis-runtime` primitives, reports to the event stream.
10d. Built-ins — link flap, latency, packet loss, rack partition, node kill.

**Done when:** full catalogue of chaos scenarios expressible and executable; demo-library scenarios work unchanged.

---

## Phase 11 — Demo Library
**Sequential.**

One demo per template, each pairing a scenario with a chaos story. Each demo: `Themisfile`, `README.md`, expected-outcome notes.

11a. `clos-3tier-<scenario>` — e.g., BGP failover when a spine dies.
11b. `three-tier-<scenario>` — e.g., distribution-pair outage.
11c. `hub-spoke-<scenario>` — e.g., branch-link flap with route reconvergence.

**Done when:** each demo runs clean from `cd demo && themis deploy` to a working lab with its chaos scenario verified.

---

## Phase 12 — Documentation
**Sequential finalization. Inline rustdoc grows continuously throughout earlier phases.**

12a. `docs/USER_GUIDE.md` — getting started, Themisfile reference, common workflows.
12b. `docs/CHAOS_DSL.md` — grammar, primitives, examples.
12c. `docs/INSTALLATION.md` — prerequisites, install flow, uninstall.
12d. `README.md` — final reconciliation against the shipped product.

**Done when:** a stranger cloning the repo can install, run, and extend Themis without contacting the author.

---

## Phase 13 — Distribution
**Sequential.**

13a. Release binary build — musl static linking where possible; single artifact per target.
13b. `install.sh` — detects platform, downloads, verifies checksum, installs to `~/.local/bin`.
13c. `.github/workflows/release.yml` — tag triggers build and GitHub release publish.
13d. `cargo about` generates `THIRD_PARTY_LICENSES` at release time; committed to the release tarball.
13e. SHA256 checksums published alongside binaries.

**Done when:** `curl -sSL <url>/install.sh | sh` on a clean machine lands working binaries and verifies integrity.

---

## Phase 14 — Release
**Sequential. Final.**

14a. End-to-end test on a clean machine (VM snapshot or fresh Fedora install).
14b. License audit — `cargo-deny` green, `THIRD_PARTY_LICENSES` spot-checked.
14c. Documentation review — walk the tree as a stranger would.
14d. Announcement drafts — NANOG, SREcon, lobste.rs, HN, network-engineering lists.
14e. Tag release. Publish. Post.

**Done when:** Themis is released. Then: active-maintainer search per VISION.md for 3–6 months, then step away.

---

## Parallelism map

```
Phase 0 (serial)
   │
Phase 1 (parallel sub-tasks)
   │
Phase 2 (2a || 2b → 2c)
   │
┌──┴───────────────────────────┐
│                              │
Phase 3 (parallel within)   Phase 4 (parallel within)
│                              │
└──────────┬───────────────────┘
           │
┌──────────┴──────────┐
│                     │
Phase 5              Phase 6
(3 agents parallel)  (2 agents parallel)
│                     │
└──────────┬──────────┘
           │
       Phase 7 (sequential)
           │
       Phase 8 (sequential)
           │
       Phase 9 (sequential) ─┐
           │                 │ can run parallel if agents available
       Phase 10 (sequential)─┘
           │
       Phase 11 (sequential)
           │
       Phase 12 (sequential finalization)
           │
       Phase 13 (sequential)
           │
       Phase 14 (sequential)
```

---

*Work plan locked. Scope does not grow.*

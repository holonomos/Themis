# Themis — Vision

A declarative data-center-fabric lab. One Linux host, a short project file, and you have a running routed fabric with realistic BGP, a working control plane, and composable chaos. Built deliberately. Released once. Walked away from.

## Who it's for

Enterprise IT — network engineers, SREs, and architects who run BGP, need to rehearse changes before prod, and don't have a dedicated test lab. Protocol researchers with hypotheses to test against real routing stacks. People who work in code, not GUIs.

Not for hyperscalers. They have their own.

## What it is

- Single-host KVM-based network fabric lab.
- Declarative topology: a project file describes the fabric; Themis makes it real.
- Real routing: FRR and Cumulus VX, not toy simulators.
- Real chaos: link flap, latency, packet loss, partitions, node kill — as composable incantations.
- Single static artifact: one binary + SQLite. `curl | sh` install, no runtime dependencies beyond the host's KVM/libvirt/QEMU.
- Fully permissive: MIT OR Apache-2.0, enforced in CI by `cargo-deny`.

## What it is not

- Not a DC-scale emulator. Single-host, enterprise-sized fabrics.
- Not a GUI tool. TUI is the surface.
- Not a living project. One release. No v1.1, no v2.
- Not incremental. Scope is locked; features fall out of the build, not into it.
- Not a product with users, maintainers, or a roadmap post-release. After it ships, it's the community's.

## Architecture

Three binaries, one project:

- **themisd** — standalone userspace daemon. SQLite-backed state. gRPC over unix socket.
- **themis** — CLI client. Speaks gRPC. Same capabilities as the TUI.
- **themis-tui** — Rust TUI (ratatui). Live state, topology rendering, chaos surface, inspector. The visual statement.

Runtime: Rust-native orchestration via `russh` + `tokio` + shelled `virsh`. No Ansible. No Python.

Compiler: Rust generator producing libvirt XML, cloud-init seeds, and NOS configs via `minijinja`.

## Scope

**Templates (three, final):**
- `clos-3tier` — kept from current scaffold.
- `three-tier` — traditional core / distribution / access. New.
- `hub-spoke` — enterprise multi-branch. New.

**Platforms (two, final):**
- FRR on Fedora.
- Cumulus VX / NVUE.

**Explicitly out:** clos-5stage, larger DC topologies, multi-site interconnect (users compose instances themselves), orchestrator/registry/telemetry stubs from earlier scaffolding, iterative releases, migration or version-compatibility stories.

## Aesthetic

Weighty, not ornamental. Earned through deliberation, not dressed up. TUI visuals dense and considered — ratatui and Braille density do the work. Language is spare. Domain vocabulary lives in templates (each template owns its nouns). The mythological name stays quiet.

## The project file

The on-disk artifact that defines a fabric. Name, filename, and syntax designed deliberately. Must read like a sentence an engineer would paste into a Slack channel and be proud of. Becomes a noun.

## What ships in the release

- Three binaries: `themisd`, `themis`, `themis-tui`.
- Vision doc (this one), architecture doc, user guide, chaos DSL reference.
- Demo library — one curated example per template and per feature category. For competent practitioners, not learners.
- `THIRD_PARTY_LICENSES` file (auto-generated from `Cargo.lock`).

## The finish line

Themis is done when:

1. All three templates work end-to-end against FRR (and Cumulus VX where applicable).
2. `themisd` is feature-complete, crash-free under normal use, and reconciles state cleanly across restarts.
3. CLI and TUI are feature-interchangeable, speaking the full gRPC surface.
4. Chaos DSL is composable and covers the documented scenarios.
5. Project file format is stable and documented.
6. Documentation is release-quality, self-contained.
7. Demo library is complete.
8. `cargo-deny` is green in CI.
9. `curl | sh` lands a working binary on a clean machine with only KVM/libvirt/QEMU prerequisites.
10. A stranger can go from clone to running fabric without contacting the author.

## After release

Public announcement through relevant channels (NANOG, SREcon, lobste.rs, HN, network-engineering community lists). Active maintainer search for three to six months: review PRs, onboard candidates, transfer ownership when a qualified volunteer commits. Then step away. No continued active development. Project belongs to whoever inherits.

---

*Vision locked. No scope additions past this document.*

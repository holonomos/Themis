# Themis

Themis is a topology-driven KVM network fabric emulator. A single Linux host runs a
full routed data-center fabric — borders, spines, leafs, servers — plus a fixed
control plane (bastion, services, orchestrator, telemetry, registry), all as VMs on
libvirt/KVM. The aggressive use of KSM (Kernel Samepage Merging) across a shared
base image makes large fabrics viable on a workstation.

A Python generator compiles a short `project.yml` (template + parameters + platform)
into an Ansible inventory and per-node NOS configuration files. Static Ansible
playbooks then provision, wire, and configure the lab.

## Status

This is the initial scaffold.

Implemented:
- **Template:** `clos-3tier` (borders, spines, leafs, servers)
- **Platform:** `frr-fedora` (FRR on Fedora cloud base)
- Generator pipeline (loader → expander → estimator → inventory → renderer)
- Static Ansible playbooks (`deploy.yml`, `teardown.yml`, `day2-push-config.yml`)
- Chaos tasks (link up/down/flap, latency, packet loss, node kill, rack partition)
- Golden-image baker (self-contained Vagrant bootstrap)

Stubbed / deferred:
- Templates `clos-5stage`, `hub-spoke`
- Control-plane roles `telemetry`, `orchestrator`, `registry` (bastion + services are implemented)
- Test scaffold

## Quickstart

Prerequisites: KVM, libvirt, Vagrant (with `vagrant-libvirt`), Ansible, Python 3.9+,
and a Fedora cloud base box.

```bash
pip install -r requirements.txt
ansible-galaxy install -r ansible-requirements.yml

# 1. Bake a golden base image.
python -m cli.main bake --platform frr-fedora --base /path/to/fedora.box
python -m cli.main bake --package
# → golden-bootstrap/golden-image.box

# 2. Create a project.
python -m cli.main init
# → prompts for project_name, template, parameters, platform, wan_interface
# → writes project.yml in $CWD

# 3. Generate the inventory + configs.
python -m cli.main generate
# → generated/inventory/{hosts.yml, group_vars/, host_vars/}
# → generated/configs/<node>/{frr.conf, daemons, vtysh.conf, 70-fabric.rules}

# 4. Deploy the lab.
python -m cli.main deploy
```

Other commands: `themis estimate` (print RAM/vCPU plan with KSM savings),
`themis teardown`, `themis push-config` (re-push NOS configs to a running lab),
`themis platforms list`, `themis templates list`.

## Architecture

```
project.yml ──► generator/ ──► generated/inventory/*.yml
                     │              generated/configs/<node>/*
                     │
                     └──► platforms/<name>/nos-driver/driver.py
                                  ├── generate_config(node, topology, env)
                                  └── node_roles() -> list[str]

static:          ansible/deploy.yml → vm-provision + l1-fabric + control-plane
                                      + nos-dispatch + observability
                 ansible/roles/nos-dispatch/tasks/<nos>/push.yml (per-NOS)
```

The generator is the compiler — it only produces data (inventory + config files).
Ansible is the runtime. The playbooks (`deploy.yml`, `teardown.yml`,
`day2-push-config.yml`) are **static**; they read the generated inventory and never
change per-project.

## Extending

### New topology template

Add a directory under `templates/` with:
- `template.yml` — parameter schema, fixed counts, addressing, ASN scheme
- `expander.py` — function `expand(template_name, parameters, templates_dir) -> dict`

The returned topology dict must contain `nodes`, `links`, `management`, `addressing`.

### New NOS or platform

Add a directory under `platforms/` with:
- `platform.yml` — display name, base OS, NOS, versions, resource profiles, KSM params
- `nos-driver/driver.py` implementing:
  - `generate_config(node, topology, jinja_env) -> dict[remote_path, content]`
  - `node_roles() -> list[str]`
- `nos-driver/templates/*.j2` — whatever templates the driver renders
- `image-recipe/provision.sh` — installed on the golden VM by `bake --platform`

Then add `ansible/roles/nos-dispatch/tasks/<nos_type>/push.yml` (and optional
`evpn.yml`, `verify.yml`). That's the entire Ansible-side NOS contract.

## Project layout

```
generator/     — Python compiler (loader, expander, estimator, inventory, renderer)
cli/           — Click CLI (themis)
templates/     — topology templates (clos-3tier, clos-5stage, hub-spoke)
platforms/     — NOS platforms (frr-fedora)
ansible/       — static playbooks + roles (vm-provision, l1-fabric, control-plane,
                 nos-dispatch, observability, chaos)
golden-bootstrap/ — self-contained Vagrant golden-image baker
generated/     — compiler output (gitignored)
```

## License

Apache 2.0.

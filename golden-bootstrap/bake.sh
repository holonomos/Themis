#!/usr/bin/env bash
# golden-bootstrap/bake.sh — Create a Themis golden VM image
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BOX_NAME="themis-golden-base"
DEFAULT_OUTPUT="${SCRIPT_DIR}/golden-image.box"

if [[ "${1:-}" == "--package" ]]; then
    OUTPUT="${2:-$DEFAULT_OUTPUT}"
    echo "=== Packaging golden image ==="
    cd "$SCRIPT_DIR"
    export THEMIS_VAGRANT_BOX="$BOX_NAME"
    vagrant halt golden-strap
    vagrant package golden-strap --output "$OUTPUT"
    echo "Output: $OUTPUT"
    exit 0
fi

PROVISION_SCRIPT=""
if [[ "${1:-}" == "--provision" ]]; then
    PROVISION_SCRIPT="${2:?--provision requires a path to a script}"
    shift 2
fi

BASE_BOX="${1:-}"
if [[ -z "$BASE_BOX" && -z "$PROVISION_SCRIPT" ]]; then
    read -r -p "Path to base .box: " BASE_BOX
    BASE_BOX="${BASE_BOX/#\~/$HOME}"
fi

if [[ -n "$BASE_BOX" && -f "$BASE_BOX" ]]; then
    virsh -c qemu:///system net-start vagrant-libvirt 2>/dev/null || true
    vagrant box remove "$BOX_NAME" --force 2>/dev/null || true
    vagrant box add --name "$BOX_NAME" "$BASE_BOX"
fi

cd "$SCRIPT_DIR"
export THEMIS_VAGRANT_BOX="$BOX_NAME"
vagrant up golden-strap

if [[ -n "$PROVISION_SCRIPT" ]]; then
    echo "Running provision script..."
    vagrant ssh golden-strap -- "sudo bash -s" < "$PROVISION_SCRIPT"
    echo "Provisioning complete."
    exit 0
fi

echo "=== VM is up. SSH in and install what the lab needs. ==="
echo "  vagrant ssh golden-strap"
echo "When done:"
echo "  bash bake.sh --package"

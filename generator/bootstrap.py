"""
Bootstrap seed generator.

For every node whose bootstrap.mode == "seed", writes a directory of cloud-init
seed files (meta-data, user-data, network-config) under out_dir/seeds/<name>/.
The vm-bootstrap Ansible role packs each directory into an ISO9660 image that
is attached to the VM as a second disk at domain-definition time.

DHCP-mode nodes are not given seeds; they lease from services' dnsmasq using
the MAC reservations emitted in group_vars/all.yml:dhcp_reservations.
"""

import os
import yaml


def _mgmt_ifname() -> str:
    return "eth-mgmt"


def _meta_data(node: dict, domain: str) -> str:
    return yaml.safe_dump({
        "instance-id": f"themis-{node['name']}",
        "local-hostname": f"{node['name']}.{domain}",
    }, sort_keys=False)


def _user_data(node: dict, domain: str) -> str:
    cfg = {
        "hostname": node["name"],
        "fqdn": f"{node['name']}.{domain}",
        "preserve_hostname": False,
        "manage_etc_hosts": True,
    }
    return "#cloud-config\n" + yaml.safe_dump(cfg, sort_keys=False)


def _network_config(node: dict, mgmt_cidr: str, services_ip: str, domain: str) -> str:
    prefix = mgmt_cidr.split("/")[1]
    ethernets = {
        "mgmt": {
            "match": {"macaddress": node["mgmt_mac"].lower()},
            "set-name": _mgmt_ifname(),
            "addresses": [f"{node['mgmt_ip']}/{prefix}"],
            "nameservers": {"addresses": [services_ip], "search": [domain]},
        }
    }

    for iface in node.get("interfaces", []):
        if iface.get("role") == "data":
            ethernets["data"] = {
                "match": {"macaddress": iface["mac"].lower()},
                "set-name": iface["name"],
                "addresses": [iface["ip"]],
                "routes": [{"to": "0.0.0.0/0", "via": iface["peer_ip"]}],
            }

    return yaml.safe_dump({"version": 2, "ethernets": ethernets}, sort_keys=False)


def write_seeds(topology: dict, out_dir: str) -> list:
    """
    Emit cloud-init seed content for every seed-mode node. Returns the list of
    node names that were seeded (useful for the Ansible iteration).
    """
    seeds_dir = os.path.join(out_dir, "seeds")
    os.makedirs(seeds_dir, exist_ok=True)

    management = topology["management"]
    domain = management["dns_domain"]
    mgmt_cidr = management["cidr"]

    services_ip = next(
        (n["mgmt_ip"] for n in topology["nodes"].values() if n["role"] == "services"),
        management["gateway"],
    )

    seeded = []
    for name, node in topology["nodes"].items():
        if node.get("bootstrap", {}).get("mode") != "seed":
            continue
        node_dir = os.path.join(seeds_dir, name)
        os.makedirs(node_dir, exist_ok=True)
        with open(os.path.join(node_dir, "meta-data"), "w", encoding="utf-8") as f:
            f.write(_meta_data(node, domain))
        with open(os.path.join(node_dir, "user-data"), "w", encoding="utf-8") as f:
            f.write(_user_data(node, domain))
        with open(os.path.join(node_dir, "network-config"), "w", encoding="utf-8") as f:
            f.write(_network_config(node, mgmt_cidr, services_ip, domain))
        seeded.append(name)

    return seeded

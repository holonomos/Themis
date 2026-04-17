"""
NOS Driver for Cumulus VX (NVUE)
"""

import yaml

def generate_udev_rules(node: dict) -> str:
    lines = [
        f"# Themis — udev interface naming rules for {node['name']}",
        "# Maps deterministic MACs to Cumulus VX interface names.",
        "",
    ]
    for iface in node.get("interfaces", []):
        if iface.get("mac"):
            mac_lower = iface["mac"].lower()
            lines.append(
                f'SUBSYSTEM=="net", ACTION=="add", '
                f'ATTR{{address}}=="{mac_lower}", '
                f'NAME="{iface["name"]}"'
            )
    lines.append("")
    return "\n".join(lines)

def generate_config(node: dict, topology: dict, template_env) -> dict[str, str]:
    # Udev logic
    rules = generate_udev_rules(node)

    # NVUE startup.yaml logic
    loopback_cidr = node.get("loopback", "")
    router_id = loopback_cidr.split("/")[0] if loopback_cidr else ""
    
    nvue_config = {
        "system": {
            "hostname": node.get("name")
        },
        "interface": {},
        "router": {
            "bgp": {
                "enable": "on",
                "autonomous-system": node.get("asn"),
                "router-id": router_id
            }
        },
        "vrf": {
            "default": {
                "router": {
                    "bgp": {
                        "enable": "on",
                        "address-family": {
                            "ipv4-unicast": {
                                "enable": "on",
                                "redistribute": {
                                    "connected": {
                                        "enable": "on"
                                    }
                                },
                                "maximum-paths": {
                                    "ebgp": 8
                                }
                            }
                        },
                        "neighbor": {}
                    }
                }
            }
        }
    }

    if loopback_cidr:
        nvue_config["interface"]["lo"] = {
            "ip": {
                "address": {
                    loopback_cidr: {}
                }
            },
            "type": "loopback"
        }

    for iface in node.get("interfaces", []):
        name = iface["name"]
        ip = iface.get("ip")
        if ip:
            nvue_config["interface"][name] = {
                "ip": {
                    "address": {
                        ip: {}
                    }
                },
                "type": "swp"
            }
    
    neighbors = nvue_config["vrf"]["default"]["router"]["bgp"]["neighbor"]
    for peer in node.get("bgp_neighbors", []):
        neighbors[peer["ip"]] = {
            "remote-as": peer["remote_asn"],
            "type": "numbered",
            "bfd": {
                "enable": "on",
                "detect-multiplier": 3,
                "min-rx-interval": 300,
                "min-tx-interval": 300
            }
        }

    nvue_root = [{"set": nvue_config}]
    startup_yaml = yaml.safe_dump(nvue_root, sort_keys=False, default_flow_style=False)

    return {
        "/etc/nvue.d/startup.yaml": startup_yaml,
        "/etc/udev/rules.d/70-fabric.rules": rules
    }

def node_roles() -> list[str]:
    return ["border", "spine", "leaf"]

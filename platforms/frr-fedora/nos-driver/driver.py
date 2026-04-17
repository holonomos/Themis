"""
NOS Driver for FRR on Fedora
"""

import jinja2

def _build_context(node: dict, topology: dict) -> dict:
    loopback_ip = node["loopback"].split("/")[0] if node.get("loopback") else ""
    needs_allowas_in = node["role"] in ("border", "leaf")
    
    bastion_gateways = []
    if node["role"] == "border":
        for iface in node.get("interfaces", []):
            if iface["peer"] == "bastion":
                bastion_gateways.append(iface["peer_ip"])

    server_static_routes = []
    if node["role"] == "leaf":
        for iface in node.get("interfaces", []):
            peer_name = iface["peer"]
            peer_node = topology["nodes"].get(peer_name)
            if peer_node and peer_node["role"] == "server" and peer_node.get("loopback"):
                server_static_routes.append({
                    "prefix": peer_node["loopback"],
                    "nexthop": iface["peer_ip"],
                    "server": peer_node["name"]
                })
                
    # Use hardcoded timers if not present
    timers = topology.get("timers", {
        "bfd": {"tx_interval_ms": 300, "rx_interval_ms": 300, "detect_multiplier": 3},
        "bgp": {"keepalive_s": 3, "holdtime_s": 9}
    })

    return {
        "hostname": node["name"],
        "role": node["role"],
        "asn": node["asn"],
        "router_id": loopback_ip,
        "loopback": node.get("loopback", ""),
        "interfaces": node.get("interfaces", []),
        "bgp_neighbors": node.get("bgp_neighbors", []),
        "needs_allowas_in": needs_allowas_in,
        "evpn_vtep": node.get("evpn_vtep", False),
        "bfd_tx": timers["bfd"]["tx_interval_ms"],
        "bfd_rx": timers["bfd"]["rx_interval_ms"],
        "bfd_mult": timers["bfd"]["detect_multiplier"],
        "bgp_keepalive": timers["bgp"]["keepalive_s"],
        "bgp_holdtime": timers["bgp"]["holdtime_s"],
        "is_spine": node["role"] == "spine",
        "bastion_gateways": bastion_gateways,
        "server_static_routes": server_static_routes,
    }

def generate_udev_rules(node: dict) -> str:
    lines = [
        f"# Themis — udev interface naming rules for {node['name']}",
        "# Maps deterministic MACs to FRR interface names.",
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

def generate_config(node: dict, topology: dict, template_env: "jinja2.Environment") -> dict:
    ctx = _build_context(node, topology)
    result = {}
    result["/etc/frr/frr.conf"] = template_env.get_template("frr.conf.j2").render(ctx)
    result["/etc/frr/daemons"] = template_env.get_template("daemons.j2").render(ctx)
    result["/etc/frr/vtysh.conf"] = template_env.get_template("vtysh.conf.j2").render(ctx)
    result["/etc/udev/rules.d/70-fabric.rules"] = generate_udev_rules(node)
    return result

def node_roles() -> list:
    return ["border", "spine", "leaf"]

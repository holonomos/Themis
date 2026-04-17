"""
Expander for clos-5stage topology.

Produces a topology dict (nodes, links, management, addressing) from the user-supplied
template parameters and the template's fixed addressing/ASN scheme in template.yml.
"""

from typing import Dict, Any

TIER_CODES = {
    "border":      0x01,
    "spine":       0x02,
    "leaf":        0x03,
    "server":      0x04,
    "bastion":     0x05,
    "mgmt":        0x06,
    "obs":         0x07,
    "ops":         0x08,
    "artifacts":   0x09,
    "super_spine": 0x0A,
}

def generate_mac(role: str, index: int) -> str:
    tier = TIER_CODES.get(role, 0xFF)
    return f"02:4E:57:{tier:02X}:{(index >> 8) & 0xFF:02X}:{index & 0xFF:02X}"

def generate_fabric_mac(role: str, node_index: int, peer_index: int) -> str:
    tier = TIER_CODES.get(role, 0xFF)
    return f"02:4E:57:{tier:02X}:{peer_index & 0xFF:02X}:{node_index & 0xFF:02X}"

def int_to_ip(ip_int: int) -> str:
    return f"{(ip_int >> 24) & 0xFF}.{(ip_int >> 16) & 0xFF}.{(ip_int >> 8) & 0xFF}.{ip_int & 0xFF}"

def ip_to_int(ip_str: str) -> int:
    octets = ip_str.split('.')
    return (int(octets[0]) << 24) | (int(octets[1]) << 16) | (int(octets[2]) << 8) | int(octets[3])

def expand(template_name: str, parameters: dict, templates_dir: str) -> dict:
    from generator.loader import load_template_meta
    meta = load_template_meta(templates_dir, template_name)
    
    nodes = {}
    links = []
    
    super_spine_count = parameters["super_spine_count"]
    pod_count = parameters["pod_count"]
    spines_per_pod = parameters["spines_per_pod"]
    racks_per_pod = parameters["racks_per_pod"]
    servers_per_rack = parameters["servers_per_rack"]
    
    border_count = meta["fixed"]["border_count"]
    leafs_per_rack = meta["fixed"]["leafs_per_rack"]
    control_nodes = meta["fixed"]["control_plane_nodes"]
    
    addr = meta["addressing"]
    mgmt_network_ip = ip_to_int(addr["mgmt_cidr"].split("/")[0])
    loopback_base = ip_to_int(addr["loopback_cidr"].split("/")[0])
    fabric_p2p_base = ip_to_int(addr["fabric_p2p_cidr"].split("/")[0])
    
    asn_rules = meta["asn"]
    
    role_counters = {}
    
    def add_node(name: str, role: str, is_frr: bool, mgmt_ip: str, loopback: str = None, asn: int = None, rack: int = None, pod: int = None):
        role_counters[role] = role_counters.get(role, 0) + 1
        idx = role_counters[role]
        mac = generate_mac(role, idx)
        
        nodes[name] = {
            "name": name,
            "role": role,
            "type": "frr-vm" if is_frr else "fedora-vm",
            "nos_type": "frr" if is_frr else None,
            "asn": asn,
            "loopback": loopback,
            "mgmt_ip": mgmt_ip,
            "vcpu": 1,
            "memory_mb": 256,
            "disk_gb": 3,
            "mgmt_mac": mac,
            "interfaces": [],
            "bgp_neighbors": [],
            "_role_index": idx,
            "_rack": rack,
            "_pod": pod
        }
        return nodes[name]

    # 1. Control plane nodes
    for cn in control_nodes:
        add_node(cn["name"], cn["role"], False, cn["mgmt_ip"])

    def get_mgmt_ip(offset: int) -> str:
        if offset > 254:
            raise ValueError("mgmt /24 exhausted")
        return int_to_ip(mgmt_network_ip + offset)

    # 2. Borders
    mgmt_ip_offset = 10
    border_names = []
    for i in range(1, border_count + 1):
        mgmt_ip = get_mgmt_ip(mgmt_ip_offset)
        mgmt_ip_offset += 1
        loopback = f"{int_to_ip(loopback_base + (1 << 8) + i)}/32"
        asn = asn_rules["border"]
        name = f"border-{i}"
        border_names.append(name)
        add_node(name, "border", True, mgmt_ip, loopback, asn)

    # 2.5 Super-spines
    mgmt_ip_offset = 20
    super_spine_names = []
    for i in range(1, super_spine_count + 1):
        mgmt_ip = get_mgmt_ip(mgmt_ip_offset)
        mgmt_ip_offset += 1
        loopback = f"{int_to_ip(loopback_base + (2 << 8) + i)}/32"
        asn = asn_rules["super_spine_base"] + (i - 1)
        name = f"ss-{i}"
        super_spine_names.append(name)
        add_node(name, "super_spine", True, mgmt_ip, loopback, asn)

    # 3. Spines
    mgmt_ip_offset = 30
    spine_global_idx = 1
    spine_names = []
    for p in range(1, pod_count + 1):
        for s in range(1, spines_per_pod + 1):
            letter = chr(ord('a') + s - 1)
            name = f"spine-{p}{letter}"
            spine_names.append((p, name))
            mgmt_ip = get_mgmt_ip(mgmt_ip_offset)
            mgmt_ip_offset += 1
            loopback = f"{int_to_ip(loopback_base + (3 << 8) + spine_global_idx)}/32"
            spine_global_idx += 1
            asn = asn_rules["spine_base"] + (p - 1) * 10 + (s - 1)
            add_node(name, "spine", True, mgmt_ip, loopback, asn, pod=p)

    # 4. Leafs
    mgmt_ip_offset = 60
    leaf_global_idx = 1
    leaf_names = []
    for p in range(1, pod_count + 1):
        for r in range(1, racks_per_pod + 1):
            for l in range(1, leafs_per_rack + 1):
                letter = chr(ord('a') + l - 1)
                name = f"leaf-{p}{r}{letter}"
                leaf_names.append((p, r, name))
                mgmt_ip = get_mgmt_ip(mgmt_ip_offset)
                mgmt_ip_offset += 1
                loopback = f"{int_to_ip(loopback_base + (4 << 8) + leaf_global_idx)}/32"
                leaf_global_idx += 1
                asn = asn_rules["leaf_base"] + (p - 1) * 100 + (r - 1)
                add_node(name, "leaf", True, mgmt_ip, loopback, asn, rack=r, pod=p)

    # 5. Servers
    mgmt_ip_offset = 120
    server_global_idx = 1
    for p in range(1, pod_count + 1):
        for r in range(1, racks_per_pod + 1):
            for s in range(1, servers_per_rack + 1):
                name = f"srv-{p}-{r}-{s}"
                mgmt_ip = get_mgmt_ip(mgmt_ip_offset)
                mgmt_ip_offset += 1
                loopback = f"{int_to_ip(loopback_base + (5 << 8) + server_global_idx)}/32"
                server_global_idx += 1
                add_node(name, "server", False, mgmt_ip, loopback, rack=r, pod=p)

    # 6. Wire links
    link_idx = 0
    p2p_offset = 0
    node_peer_counters = {}
    
    def connect(a_name, b_name, tier):
        nonlocal link_idx, p2p_offset
        a_node = nodes[a_name]
        b_node = nodes[b_name]
        
        subnet_int = fabric_p2p_base + (p2p_offset * 4)
        p2p_offset += 1
        
        a_ip = f"{int_to_ip(subnet_int + 1)}/30"
        b_ip = f"{int_to_ip(subnet_int + 2)}/30"
        subnet = f"{int_to_ip(subnet_int)}/30"
        
        bridge = f"br{link_idx:03d}"
        link_idx += 1
        
        a_ifname = f"eth-{b_name}"
        b_ifname = f"eth-{a_name}"
        
        a_fabric_mac = ""
        b_fabric_mac = ""
        
        if a_node["type"] == "frr-vm":
            node_peer_counters[a_name] = node_peer_counters.get(a_name, 0) + 1
            a_fabric_mac = generate_fabric_mac(a_node["role"], a_node["_role_index"], node_peer_counters[a_name])
            
        if b_node["type"] == "frr-vm":
            node_peer_counters[b_name] = node_peer_counters.get(b_name, 0) + 1
            b_fabric_mac = generate_fabric_mac(b_node["role"], b_node["_role_index"], node_peer_counters[b_name])
            
        a_iface = {
            "name": a_ifname, "ip": a_ip, "peer_ip": int_to_ip(subnet_int + 2),
            "subnet": subnet, "peer": b_name, "bridge": bridge, "mac": a_fabric_mac
        }
        b_iface = {
            "name": b_ifname, "ip": b_ip, "peer_ip": int_to_ip(subnet_int + 1),
            "subnet": subnet, "peer": a_name, "bridge": bridge, "mac": b_fabric_mac
        }
        
        a_node["interfaces"].append(a_iface)
        b_node["interfaces"].append(b_iface)
        
        if a_node["type"] == "frr-vm" and b_node["type"] == "frr-vm":
            a_node["bgp_neighbors"].append({"ip": int_to_ip(subnet_int + 2), "remote_asn": b_node["asn"], "name": b_name, "interface": a_ifname})
            b_node["bgp_neighbors"].append({"ip": int_to_ip(subnet_int + 1), "remote_asn": a_node["asn"], "name": a_name, "interface": b_ifname})
            
        links.append({
            "bridge": bridge, "a": a_name, "b": b_name,
            "a_ip": a_ip, "b_ip": b_ip,
            "a_ifname": a_ifname, "b_ifname": b_ifname,
            "a_mac": a_fabric_mac, "b_mac": b_fabric_mac,
            "subnet": subnet, "tier": tier
        })

    # Wiring:
    for b_name in border_names:
        connect(b_name, "bastion", "border_bastion")

    for b_name in border_names:
        for ss_name in super_spine_names:
            connect(b_name, ss_name, "border_super_spine")
            
    for ss_name in super_spine_names:
        for _, sp_name in spine_names:
            connect(ss_name, sp_name, "super_spine_spine")
            
    for p in range(1, pod_count + 1):
        p_spines = [n for pp, n in spine_names if pp == p]
        p_leafs = [n for pp, r, n in leaf_names if pp == p]
        for sp_name in p_spines:
            for l_name in p_leafs:
                connect(sp_name, l_name, "spine_leaf")
                
    for p in range(1, pod_count + 1):
        for r in range(1, racks_per_pod + 1):
            leaf_a = f"leaf-{p}{r}a"
            leaf_b = f"leaf-{p}{r}b"
            for s in range(1, servers_per_rack + 1):
                srv = f"srv-{p}-{r}-{s}"
                connect(leaf_a, srv, "leaf_server")
                connect(leaf_b, srv, "leaf_server")

    for n in nodes.values():
        n.pop("_role_index", None)
        n.pop("_rack", None)
        n.pop("_pod", None)

    management = {
        "cidr": addr["mgmt_cidr"],
        "gateway": addr["mgmt_gateway"],
        "bridge": addr["mgmt_bridge"],
        "dns_domain": "themis.local"
    }

    return {
        "nodes": nodes,
        "links": links,
        "management": management,
        "addressing": {
            "loopback_cidr": addr["loopback_cidr"],
            "fabric_p2p_cidr": addr["fabric_p2p_cidr"]
        }
    }

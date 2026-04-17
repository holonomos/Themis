import os
import yaml

def write_inventory(topology: dict, platform: dict, project: dict, out_dir: str) -> None:
    """
    Write the complete Ansible inventory to out_dir/inventory/.
    Follows the inventory schema defined in the Contracts section exactly.
    """
    inventory_dir = os.path.join(out_dir, "inventory")
    group_vars_dir = os.path.join(inventory_dir, "group_vars")
    host_vars_dir = os.path.join(inventory_dir, "host_vars")
    
    os.makedirs(group_vars_dir, exist_ok=True)
    os.makedirs(host_vars_dir, exist_ok=True)
    
    hosts = {
        "all": {
            "children": {
                "hypervisor": {
                    "hosts": {
                        "localhost": {
                            "ansible_connection": "local"
                        }
                    }
                },
                "control_plane": {
                    "hosts": {}
                },
                "fabric_nodes": {
                    "children": {
                        "borders": {"hosts": {}},
                        "spines": {"hosts": {}},
                        "leafs": {"hosts": {}},
                        "servers": {"hosts": {}}
                    }
                }
            }
        }
    }
    
    cp_hosts = hosts["all"]["children"]["control_plane"]["hosts"]
    borders = hosts["all"]["children"]["fabric_nodes"]["children"]["borders"]["hosts"]
    spines = hosts["all"]["children"]["fabric_nodes"]["children"]["spines"]["hosts"]
    leafs = hosts["all"]["children"]["fabric_nodes"]["children"]["leafs"]["hosts"]
    servers = hosts["all"]["children"]["fabric_nodes"]["children"]["servers"]["hosts"]
    
    for name, node in topology["nodes"].items():
        role = node["role"]
        entry = {"ansible_host": node["mgmt_ip"]}
        if role in ("mgmt", "bastion", "ops", "obs", "artifacts"):
            entry["role"] = role
            cp_hosts[name] = entry
        elif role == "border":
            borders[name] = entry
        elif role == "spine":
            spines[name] = entry
        elif role == "leaf":
            leafs[name] = entry
        elif role == "server":
            servers[name] = entry

    with open(os.path.join(inventory_dir, "hosts.yml"), "w", encoding="utf-8") as f:
        yaml.safe_dump(hosts, f, sort_keys=False)
        
    project_name = project.get("project_name", "themis")
    mgmt_bridge = topology["management"]["bridge"].replace("<project-name>", project_name)
    data_bridge = topology["management"]["data_bridge"].replace("<project-name>", project_name)
    all_links = []
    
    for link in topology["links"]:
        all_links.append({
            "bridge": link["bridge"],
            "a": link["a"],
            "b": link["b"],
            "a_ip": link["a_ip"],
            "b_ip": link["b_ip"],
            "subnet": link["subnet"]
        })
        
    reservations = [
        {"name": node["name"], "ip": node["mgmt_ip"]}
        for node in topology["nodes"].values()
    ]

    all_vars = {
        "platform": platform["name"],
        "project_name": project_name,
        "mgmt_bridge": mgmt_bridge,
        "mgmt_cidr": topology["management"]["cidr"],
        "mgmt_gateway": topology["management"]["gateway"],
        "data_bridge": data_bridge,
        "data_cidr": topology["management"]["data_cidr"],
        "data_gateway": topology["management"]["data_gateway"],
        "domain": topology["management"]["dns_domain"],
        "wan_interface": project.get("wan_interface", "eth0"),
        "base_image_path": project.get("base_image_path") or os.path.join(
            os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
            "golden-bootstrap", "golden-image.box"
        ),
        "all_links": all_links,
        "reservations": reservations,
    }
    with open(os.path.join(group_vars_dir, "all.yml"), "w", encoding="utf-8") as f:
        yaml.safe_dump(all_vars, f, sort_keys=False)
        
    fabric_vars = {
        "nos_type": platform["nos"],
        "verify_command": platform["nos_driver"]["verify_command"],
    }
    with open(os.path.join(group_vars_dir, "fabric_nodes.yml"), "w", encoding="utf-8") as f:
        yaml.safe_dump(fabric_vars, f, sort_keys=False)

    control_vars = {}
    with open(os.path.join(group_vars_dir, "control_plane.yml"), "w", encoding="utf-8") as f:
        yaml.safe_dump(control_vars, f, sort_keys=False)
        
    for name, node in topology["nodes"].items():
        if node["role"] in ("mgmt", "bastion", "ops", "obs", "artifacts"):
            continue

        if node.get("type") == "frr-vm":
            nos_type = node.get("nos_type") or platform["nos"]
        else:
            nos_type = None

        hvars = {
            "role": node["role"],
            "nos_type": nos_type,
            "asn": node.get("asn"),
            "loopback": node.get("loopback"),
            "vcpu": node.get("vcpu", 1),
            "memory_mb": node.get("memory_mb", 256),
            "disk_gb": node.get("disk_gb", 3),
            "mgmt_mac": node["mgmt_mac"],
            "interfaces": node.get("interfaces", []),
            "bgp_neighbors": node.get("bgp_neighbors", [])
        }
        if not hvars["nos_type"]:
            hvars.pop("nos_type")
        if not hvars["asn"]:
            hvars.pop("asn")

        with open(os.path.join(host_vars_dir, f"{name}.yml"), "w", encoding="utf-8") as f:
            yaml.safe_dump(hvars, f, sort_keys=False)

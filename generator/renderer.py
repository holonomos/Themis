import os
import importlib.util
from jinja2 import Environment, FileSystemLoader

def render_configs(topology: dict, platform: dict, platforms_dir: str, out_dir: str) -> None:
    """
    Render configuration files using the platform's nos_driver.
    """
    platform_name = platform["name"]
    driver_path = os.path.join(platforms_dir, platform_name, "nos-driver", "driver.py")
    if not os.path.exists(driver_path):
        raise FileNotFoundError(f"Driver not found at {driver_path}")
        
    spec = importlib.util.spec_from_file_location("platform_driver", driver_path)
    driver_module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(driver_module)
    
    templates_dir = os.path.join(platforms_dir, platform_name, "nos-driver", "templates")
    env = Environment(loader=FileSystemLoader(templates_dir), keep_trailing_newline=True, trim_blocks=True, lstrip_blocks=True)
    
    configs_dir = os.path.join(out_dir, "configs")
    valid_roles = driver_module.node_roles()
    
    for name, node in topology["nodes"].items():
        if not node.get("nos_type"):
            continue
        if node["role"] not in valid_roles:
            continue
            
        files_dict = driver_module.generate_config(node, topology, env)
        
        node_dir = os.path.join(configs_dir, name)
        os.makedirs(node_dir, exist_ok=True)
        
        for remote_path, content in files_dict.items():
            filename = os.path.basename(remote_path)
            local_path = os.path.join(node_dir, filename)
            with open(local_path, "w", encoding="utf-8") as f:
                f.write(content)

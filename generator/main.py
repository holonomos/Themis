import os
import shutil
import click
from .loader import load_project, load_platform, load_template_meta, validate_parameters
from .expander import expand
from .estimator import estimate, print_estimate
from .inventory import write_inventory
from .renderer import render_configs
from .bootstrap import write_seeds

def run(project_path: str, output_dir: str = "generated") -> None:
    """
    Orchestrates the full generation process.
    """
    base_dir = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    platforms_dir = os.path.join(base_dir, "platforms")
    templates_dir = os.path.join(base_dir, "templates")
    
    print(f"Loading project from {project_path} ...")
    project = load_project(project_path)
    platform = load_platform(platforms_dir, project["platform"])
    template_meta = load_template_meta(templates_dir, project["template"])
    parameters = validate_parameters(project["parameters"], template_meta)
    
    print("Expanding topology ...")
    topology = expand(project["template"], parameters, templates_dir)
    
    est = estimate(topology, platform)
    print_estimate(est)
    
    if not est["fits"]:
        click.confirm("Resource estimate exceeds host capacity. Continue anyway?", abort=True)
        
    if os.path.exists(output_dir):
        shutil.rmtree(output_dir)
    os.makedirs(output_dir, exist_ok=True)
    
    print("Writing inventory ...")
    write_inventory(topology, platform, project, output_dir)
    print(f"  -> {os.path.join(output_dir, 'inventory')}")
    
    print("Rendering NOS configs ...")
    render_configs(topology, platform, platforms_dir, output_dir)
    print(f"  -> {os.path.join(output_dir, 'configs')}")

    print("Writing cloud-init seeds ...")
    seeded = write_seeds(topology, output_dir)
    print(f"  -> {os.path.join(output_dir, 'seeds')} ({len(seeded)} nodes)")

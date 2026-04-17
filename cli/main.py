import os
import subprocess
import click
import yaml

@click.group()
def cli():
    """Themis: Greenfield Network Fabric Emulator"""
    pass

@cli.command()
def init():
    """Interactive wizard to create project.yml"""
    base_dir = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    templates_dir = os.path.join(base_dir, "templates")
    platforms_dir = os.path.join(base_dir, "platforms")
    
    templates = [d for d in os.listdir(templates_dir) if os.path.isdir(os.path.join(templates_dir, d))]
    platforms = [d for d in os.listdir(platforms_dir) if os.path.isdir(os.path.join(platforms_dir, d))]

    project_name = click.prompt("Project name (used for bridge and image naming)")

    click.echo(f"Available templates: {', '.join(templates)}")
    template = click.prompt("Choose template", type=click.Choice(templates))
    
    from generator.loader import load_template_meta
    meta = load_template_meta(templates_dir, template)
    
    parameters = {}
    for p_name, p_schema in meta.get("parameters", {}).items():
        default = p_schema.get("default")
        val = click.prompt(f"Parameter '{p_name}'", default=default, type=int if p_schema.get("type") == "integer" else str)
        parameters[p_name] = val
        
    click.echo(f"Available platforms: {', '.join(platforms)}")
    platform = click.prompt("Choose platform", type=click.Choice(platforms))
    
    wan_interface = click.prompt("WAN interface", default="eth0")
    
    project = {
        "project_name": project_name,
        "template": template,
        "parameters": parameters,
        "platform": platform,
        "wan_interface": wan_interface
    }
    
    with open("project.yml", "w", encoding="utf-8") as f:
        yaml.safe_dump(project, f, sort_keys=False)
    click.echo("Created project.yml")

@cli.command()
def estimate():
    """Run resource estimation"""
    if not os.path.exists("project.yml"):
        click.echo("project.yml not found. Run 'themis init' first.", err=True)
        return
        
    base_dir = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    from generator.loader import load_project, load_platform, load_template_meta, validate_parameters
    from generator.expander import expand
    from generator.estimator import estimate as _estimate, print_estimate
    
    project = load_project("project.yml")
    platform = load_platform(os.path.join(base_dir, "platforms"), project["platform"])
    template_meta = load_template_meta(os.path.join(base_dir, "templates"), project["template"])
    parameters = validate_parameters(project["parameters"], template_meta)
    
    topology = expand(project["template"], parameters, os.path.join(base_dir, "templates"))
    est = _estimate(topology, platform)
    print_estimate(est)

@cli.command()
def generate():
    """Generate configs and inventory"""
    if not os.path.exists("project.yml"):
        click.echo("project.yml not found. Run 'themis init' first.", err=True)
        return
        
    from generator.main import run as gen_run
    gen_run(os.path.abspath("project.yml"), "generated")

def _run_playbook(playbook_path: str, extra_vars=None):
    inventory_path = "generated/inventory/hosts.yml"
    if not os.path.exists(inventory_path):
        click.echo(f"Inventory {inventory_path} not found. Run 'themis generate' first.", err=True)
        return
    
    cmd = ["ansible-playbook", playbook_path, "-i", inventory_path]
    if extra_vars:
        for k, v in extra_vars.items():
            cmd.extend(["-e", f"{k}={v}"])
    subprocess.run(cmd, check=True)

@cli.command()
@click.option("--base-image", "base_image", type=click.Path(exists=True, dir_okay=False), default=None,
              help="Override base image path (overrides project.yml default).")
def deploy(base_image):
    """Deploy the fabric"""
    base_dir = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    extra = {"base_image_path": os.path.abspath(base_image)} if base_image else None
    _run_playbook(os.path.join(base_dir, "ansible", "deploy.yml"), extra_vars=extra)

@cli.command()
def teardown():
    """Teardown the fabric"""
    base_dir = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    _run_playbook(os.path.join(base_dir, "ansible", "teardown.yml"))

@cli.command("push-config")
def push_config():
    """Re-push configs to running nodes"""
    base_dir = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    _run_playbook(os.path.join(base_dir, "ansible", "day2-push-config.yml"))

@cli.group()
def platforms():
    """Manage platforms"""
    pass

@platforms.command("list")
def list_platforms():
    base_dir = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    platforms_dir = os.path.join(base_dir, "platforms")
    click.echo(f"{'NAME':<20} | {'DISPLAY NAME':<30} | {'BASE OS':<15} | {'NOS'}")
    click.echo("-" * 80)
    for p in os.listdir(platforms_dir):
        if os.path.isdir(os.path.join(platforms_dir, p)):
            try:
                from generator.loader import load_platform
                meta = load_platform(platforms_dir, p)
                click.echo(f"{meta['name']:<20} | {meta['display_name']:<30} | {meta['base_os']:<15} | {meta['nos']}")
            except Exception:
                click.echo(f"{p:<20} | [Error loading platform.yml]")

@cli.group()
def templates():
    """Manage templates"""
    pass

@templates.command("list")
def list_templates():
    base_dir = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    templates_dir = os.path.join(base_dir, "templates")
    click.echo(f"{'NAME':<20} | {'DISPLAY NAME':<40} | {'PARAMETERS'}")
    click.echo("-" * 80)
    for t in os.listdir(templates_dir):
        if os.path.isdir(os.path.join(templates_dir, t)):
            try:
                from generator.loader import load_template_meta
                meta = load_template_meta(templates_dir, t)
                params_str = ", ".join(meta.get("parameters", {}).keys())
                click.echo(f"{meta['name']:<20} | {meta['display_name']:<40} | {params_str}")
            except Exception:
                click.echo(f"{t:<20} | [Error loading template.yml]")

@cli.command()
@click.option("--platform", help="Platform name to provision")
@click.option("--package", is_flag=True, help="Package the image")
def bake(platform, package):
    """Bake a base image"""
    base_dir = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    golden_dir = os.path.join(base_dir, "golden-bootstrap")
    
    cmd = ["bash", "bake.sh"]
    if package:
        cmd.append("--package")
    elif platform:
        provision_script = os.path.join(base_dir, "platforms", platform, "image-recipe", "provision.sh")
        if not os.path.exists(provision_script):
            click.echo(f"Provision script not found: {provision_script}", err=True)
            return
        cmd.extend(["--provision", provision_script])
        
    subprocess.run(cmd, cwd=golden_dir, check=True)

if __name__ == "__main__":
    cli()

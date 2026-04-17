import os
import yaml


def _load_yaml(path: str) -> dict:
    if not os.path.isfile(path):
        raise FileNotFoundError(f"Missing file: {path}")
    with open(path, "r", encoding="utf-8") as f:
        data = yaml.safe_load(f)
    if not isinstance(data, dict):
        raise ValueError(f"File {path} must contain a YAML mapping.")
    return data


def load_project(path: str) -> dict:
    """Load project.yml. Validate required keys: template, parameters, platform, wan_interface."""
    data = _load_yaml(path)
    required_keys = {"template", "parameters", "platform", "wan_interface"}
    missing = required_keys - set(data.keys())
    if missing:
        raise ValueError(f"project.yml is missing required keys: {', '.join(missing)}")
    if data["parameters"] is not None and not isinstance(data["parameters"], dict):
        raise ValueError("project.yml 'parameters' must be a dictionary.")
    
    if data["parameters"] is None:
        data["parameters"] = {}
        
    return data


def load_platform(platforms_dir: str, name: str) -> dict:
    """Load platforms/<name>/platform.yml. Validate schema against platform.yml contract."""
    path = os.path.join(platforms_dir, name, "platform.yml")
    data = _load_yaml(path)
    
    required_keys = {"name", "display_name", "base_os", "base_os_version", "nos", "versions", "resource_profiles", "ksm", "nos_driver"}
    missing = required_keys - set(data.keys())
    if missing:
        raise ValueError(f"platform.yml for {name} missing keys: {', '.join(missing)}")
        
    return data


def load_template_meta(templates_dir: str, name: str) -> dict:
    """Load templates/<name>/template.yml. Validate parameter schema."""
    path = os.path.join(templates_dir, name, "template.yml")
    data = _load_yaml(path)
    
    required_keys = {"name", "display_name", "parameters", "fixed", "addressing", "asn"}
    missing = required_keys - set(data.keys())
    if missing:
        raise ValueError(f"template.yml for {name} missing keys: {', '.join(missing)}")
        
    return data


def validate_parameters(params: dict, template_meta: dict) -> dict:
    """
    Apply defaults and validate user parameters against template schema.
    Raise ValueError with a clear message for any violation.
    Returns the complete parameter dict with defaults filled in.
    """
    schema = template_meta.get("parameters", {})
    validated = {}
    
    # Handle the case where params is strictly None from parsed YAML
    if params is None:
        params = {}
    
    for param_name, param_schema in schema.items():
        val = params.get(param_name)
        if val is None:
            if "default" in param_schema:
                val = param_schema["default"]
            else:
                raise ValueError(f"Parameter '{param_name}' is required but missing.")
                
        # Type validation
        p_type = param_schema.get("type")
        if p_type == "integer":
            if not isinstance(val, int):
                try:
                    val = int(val)
                except ValueError:
                    raise ValueError(f"Parameter '{param_name}' must be an integer, got {type(val).__name__}.")
        
        # Min/Max validation
        if "min" in param_schema and val < param_schema["min"]:
            raise ValueError(f"Parameter '{param_name}' must be >= {param_schema['min']}.")
        if "max" in param_schema and val > param_schema["max"]:
            raise ValueError(f"Parameter '{param_name}' must be <= {param_schema['max']}.")
            
        validated[param_name] = val
        
    # Check for extra parameters provided by user but not in schema
    extra = set(params.keys()) - set(schema.keys())
    if extra:
        raise ValueError(f"Unknown parameters provided: {', '.join(extra)}")
        
    return validated

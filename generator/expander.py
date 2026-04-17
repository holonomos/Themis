import importlib.util
import os

def expand(template_name: str, parameters: dict, templates_dir: str) -> dict:
    """
    Import templates/<template_name>/expander.py and call its expand() function.
    Returns the full topology dict.
    """
    expander_path = os.path.join(templates_dir, template_name, "expander.py")
    if not os.path.exists(expander_path):
        raise FileNotFoundError(f"Expander not found at {expander_path}")
        
    spec = importlib.util.spec_from_file_location("template_expander", expander_path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    
    return module.expand(template_name, parameters, templates_dir)

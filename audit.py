import os
import re

with open('AGENT_BUILD_SPEC.md') as f:
    spec = f.read()

tree_section_match = re.search(r'├── LICENSE(.*?)\n135: Contracts', spec, re.DOTALL)
if tree_section_match:
    lines = tree_section_match.group(0).split('\n')
else:
    # Try another way
    in_tree = False
    lines = []
    for line in spec.split('\n'):
        if 'Create this exact tree' in line:
            in_tree = True
        elif 'Contracts' in line and in_tree:
            break
        elif in_tree:
            lines.append(line)

files_to_check = []
current_path = []
for line in lines:
    idx = line.find('─ ')
    if idx == -1:
        idx = line.find('  ')
    if idx == -1:
        continue
    parts = line.split('#')
    name = parts[0].strip('─ |├└\t ').strip()
    if not name:
        continue
        
    level = len(re.match(r'^[│\s]*', line).group(0)) // 4
    
    current_path = current_path[:level]
    current_path.append(name.replace('/', ''))
    
    full_path = os.path.join(*current_path)
    
    if '.' in name or 'Makefile' in name or 'LICENSE' in name or 'Vagrantfile' in name or name == 'daemons':
        files_to_check.append(full_path)

print("Missing or empty files:")
for f in files_to_check:
    if not os.path.exists(f):
        print(f"MISSING: {f}")
    elif os.path.getsize(f) == 0 and not '__init__' in f:
        print(f"EMPTY: {f}")


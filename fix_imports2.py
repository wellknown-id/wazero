import os
import re

def clean_file(fpath):
    if not os.path.exists(fpath): return
    with open(fpath, 'r') as f:
        content = f.read()

    orig = content
    content = re.sub(r'\s*experimentalsys\n', '\n', content)
    content = re.sub(r'\s*internalsys\n', '\n', content)
    
    if orig != content:
        with open(fpath, 'w') as f:
            f.write(content)

for root, dirs, files in os.walk('.'):
    for name in files:
        if name.endswith('.go'):
            clean_file(os.path.join(root, name))

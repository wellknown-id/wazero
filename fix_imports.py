import os
import re

import_patterns = [
    r'\s*"github\.com/tetratelabs/wazero/sys"\n',
    r'\s*"github\.com/tetratelabs/wazero/internal/sys"\n',
    r'\s*"github\.com/tetratelabs/wazero/experimental/sys"\n',
    r'\s*internalsys "github\.com/tetratelabs/wazero/internal/sys"\n',
    r'\s*experimentalsys "github\.com/tetratelabs/wazero/experimental/sys"\n'
]

def clean_file(fpath):
    if not os.path.exists(fpath): return
    with open(fpath, 'r') as f:
        content = f.read()
    
    orig = content
    for p in import_patterns:
        content = re.sub(p, '\n', content)
        
    if orig != content:
        with open(fpath, 'w') as f:
            f.write(content)

for root, dirs, files in os.walk('.'):
    for name in files:
        if name.endswith('.go'):
            clean_file(os.path.join(root, name))

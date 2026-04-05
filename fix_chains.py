import os
import re

def fix_file(path):
    with open(path, "r") as f:
        data = f.read()
    
    orig = data
    
    # Strip removed methods from ModuleConfig
    methods_to_strip = [
        "WithSysNanotime\([^)]*\)",
        "WithSysWalltime\([^)]*\)",
        "WithSysNanosleep\([^)]*\)",
        "WithSysOsyield\([^)]*\)",
        "WithRandSource\([^)]*\)",
        "WithFSConfig\([^)]*\)",
        "WithEnv\([^)]*\)",
        "WithArgs\([^)]*\)",
        "WithStartFunctions\([^)]*\)",
        "WithStdout\([^)]*\)",
        "WithStdin\([^)]*\)",
        "WithStderr\([^)]*\)",
        "WithFS\([^)]*\)"
    ]
    
    for m in methods_to_strip:
        # We need to handle nested parenthesis carefully but here the args are usually simple
        data = re.sub(r'\.' + m, '', data)
        # Also handle multiline chaining properly if possible, but usually it's just .WithX(...)
    
    if orig != data:
        with open(path, "w") as f:
            f.write(data)

for root, dirs, files in os.walk('.'):
    for n in files:
        if n.endswith('.go'):
            fix_file(os.path.join(root, n))

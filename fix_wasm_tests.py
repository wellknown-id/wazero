import os
import re

files_to_fix = [
    'internal/wasm/module_instance_test.go',
    'internal/wasm/store_test.go',
    'internal/wasm/global_test.go'
]

for fpath in files_to_fix:
    if os.path.exists(fpath):
        with open(fpath, 'r') as f:
            content = f.read()

        # Fix s.Instantiate(..., nil, nil) -> s.Instantiate(..., nil)
        # We know the old signature was s.Instantiate(ctx, module, name, sysCtx, typeIDs)
        content = re.sub(r'(s\.Instantiate\([^,]+, [^,]+, [^,]+), [^,]+, ([^)]+)\)', r'\1, \2)', content)

        # Fix ModuleInstance{... Sys: sysCtx ...}
        # just remove Sys field assignment completely.
        content = re.sub(r'Sys:\s*[^,{}]+,?', '', content)

        # Also remove any Sys: nil, etc
        content = re.sub(r'Sys:\s*nil,?', '', content)
        content = re.sub(r'tc.m.Sys', 'nil', content) # if tested
        content = re.sub(r'cc.Sys', 'nil', content) # if tested
        content = re.sub(r'mod.Sys', 'nil', content) # if tested

        with open(fpath, 'w') as f:
            f.write(content)

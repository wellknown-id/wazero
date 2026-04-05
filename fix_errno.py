import os
import re

for root, dirs, files in os.walk('.'):
    for n in files:
        if n.endswith('.go'):
            p = os.path.join(root, n)
            with open(p, 'r') as f:
                d = f.read()
            d2 = d.replace('require.EqualErrno', 'require.Equal')
            # Extra cleanup for WithStartFunctions that wasn't caught
            d2 = re.sub(r'\.WithStartFunctions\([^)]*\)', '', d2)
            d2 = re.sub(r'\.WithStdin\([^)]*\)', '', d2)
            d2 = re.sub(r'\.WithStdout\([^)]*\)', '', d2)
            d2 = re.sub(r'\.WithStderr\([^)]*\)', '', d2)
            
            if d != d2:
                with open(p, 'w') as f:
                    f.write(d2)

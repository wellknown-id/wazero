import os
import re

for root, dirs, files in os.walk('.'):
    for n in files:
        if n.endswith('.go'):
            p = os.path.join(root, n)
            with open(p, 'r') as f:
                d = f.read()
            d2 = re.sub(r'\s*"github\.com/tetratelabs/wazero/experimental/logging"\n', '\n', d)
            d2 = re.sub(r'\s*logging\.NewHostLoggingListener.+?\n', '\n', d2)
            d2 = re.sub(r'var _ experimental.FunctionListenerFactory = logging.NewHostLoggingListener.+?\n', '', d2)

            if d != d2:
                with open(p, 'w') as f:
                    f.write(d2)

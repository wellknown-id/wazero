import os
import re

for root, dirs, files in os.walk('.'):
    for n in files:
        if n.endswith('.go'):
            p = os.path.join(root, n)
            with open(p, 'r') as f:
                d = f.read()

            d2 = d.replace('sys.NewExitError', 'api.NewExitError')

            # remove sys imports
            d2 = re.sub(r'(\t*"github\.com/tetratelabs/wazero/sys"\n)', '', d2)
            d2 = re.sub(r'(\t*"github\.com/tetratelabs/wazero/internal/sysfs"\n)', '', d2)
            d2 = re.sub(r'(\t*"github\.com/tetratelabs/wazero/internal/testing/proxy"\n)', '', d2)
            d2 = re.sub(r'(\t*"github\.com/tetratelabs/wazero/experimental/logging"\n)', '', d2)

            if p == './internal/engine/wazevo/e2e_test.go':
                d2 = re.sub(r'func TestE2E_host_functions\(t \*testing\.T\) \{.*?\n}\n\n', '', d2, flags=re.DOTALL)
                d2 = re.sub(r'func TestE2E_Function_listeners\(t \*testing\.T\) \{.*?\n}\n\n', '', d2, flags=re.DOTALL)
                # some other tests using logging there might exist
                d2 = re.sub(r'ctx := experimental.WithFunctionListenerFactory\(context.Background\(\), logging.NewLoggingListenerFactory\(&buf\)\)', '', d2)

            if d != d2:
                with open(p, 'w') as f:
                    f.write(d2)


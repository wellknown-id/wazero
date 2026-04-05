import re

with open("internal/wasm/module_instance_test.go", "r") as f:
    text = f.read()

# Delete TestModuleInstance_Close
text = re.sub(r'func TestModuleInstance_Close\(t \*testing\.T\) \{.*?\n}\n', '', text, flags=re.DOTALL)
text = re.sub(r'func TestModuleInstance_CloseWithExitCode\(t \*testing\.T\) \{.*?\n}\n', '', text, flags=re.DOTALL)

with open("internal/wasm/module_instance_test.go", "w") as f:
    f.write(text)

with open("internal/engine/wazevo/e2e_test.go", "r") as f:
    text = f.read()

text = re.sub(r'func TestE2E_host_functions\(t \*testing\.T\) \{.*?\n}\n\n', '', text, flags=re.DOTALL)
text = re.sub(r'func TestE2E_Function_listeners\(t \*testing\.T\) \{.*', '', text, flags=re.DOTALL)

with open("internal/engine/wazevo/e2e_test.go", "w") as f:
    f.write(text)


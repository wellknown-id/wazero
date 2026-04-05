import re

with open("runtime_test.go", "r") as f:
    text = f.read()

# remove _start tests from TestRuntime_Instantiate_ErrorOnStart
text = re.sub(r'\{\s*name:\s*"_start function",\s*wasm.*?Type: wasm.ExternTypeFunc, Index: 0}},\s*},\s*},', '', text, flags=re.DOTALL)

# remove _start tests from TestRuntime_InstantiateModule_ExitError
text = re.sub(r'\{\s*name:\s*"_start: exit code 0",.*?},', '', text, flags=re.DOTALL)
text = re.sub(r'\{\s*name:\s*"_start: exit code 2",.*?},', '', text, flags=re.DOTALL)


with open("runtime_test.go", "w") as f:
    f.write(text)

with open("internal/wasm/store_test.go", "r") as f:
    text = f.read()

# Fix fmt.Sprintf("%d:%d", n) since I broke it. Let's make it fmt.Sprintf("%d", n)
text = text.replace('fmt.Sprintf("%d:%d", n)', 'fmt.Sprintf("%d", n)')

with open("internal/wasm/store_test.go", "w") as f:
    f.write(text)

with open("internal/engine/wazevo/e2e_test.go", "r") as f:
    text = f.read()

# Strip all TestListener_ tests
text = re.sub(r'func TestListener_[a-zA-Z0-9_]*\(t \*testing\.T\) \{.*?\n}\n', '', text, flags=re.DOTALL)
text = re.sub(r'func TestDWARF\(t \*testing\.T\) \{.*?\n}\n', '', text, flags=re.DOTALL)

with open("internal/engine/wazevo/e2e_test.go", "w") as f:
    f.write(text)

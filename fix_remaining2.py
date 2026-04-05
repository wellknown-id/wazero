import re

with open("internal/wasm/store_test.go", "r") as f:
    text = f.read()

# Replace any occurrence of s.Instantiate(testCtx, M, N, nil, []FunctionTypeID{0}) with s.Instantiate(testCtx, M, N, []FunctionTypeID{0})
text = re.sub(r's\.Instantiate\(([^,]+),\s*([^,]+),\s*([^,]+),\s*nil,\s*([^)]+)\)', r's.Instantiate(\1, \2, \3, \4)', text)

with open("internal/wasm/store_test.go", "w") as f:
    f.write(text)

with open("internal/wasm/module_instance_test.go", "r") as f:
    text = f.read()
    
# Remove imports of sysfs etc because I'll remove the subtests manually
text = re.sub(r'\t.*?testfs.*?\n', '', text)
text = re.sub(r'\t.*?hammer.*?\n', '', text)

with open("internal/wasm/module_instance_test.go", "w") as f:
    f.write(text)


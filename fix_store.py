import re

with open("internal/wasm/store_test.go", "r") as f:
    lines = f.readlines()

for i in range(len(lines)):
    lines[i] = lines[i].replace(', nil, []FunctionTypeID', ', []FunctionTypeID')
    lines[i] = lines[i].replace(', sys.DefaultContext(nil), []FunctionTypeID', ', []FunctionTypeID')

with open("internal/wasm/store_test.go", "w") as f:
    f.writelines(lines)
    

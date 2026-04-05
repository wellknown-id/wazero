import re

with open("internal/engine/wazevo/e2e_test.go", "r") as f:
    text = f.read()

# Replace undefined ctx caused by previous deletion
text = re.sub(r'func TestE2E_host_functions\(t \*testing\.T\) \{\n\tvar buf bytes\.Buffer\n\n\tfor _, tc := range \[\]struct \{', 'func TestE2E_host_functions(t *testing.T) {\n\tvar buf bytes.Buffer\n\tctx := context.Background()\n\tfor _, tc := range []struct {', text)

with open("internal/engine/wazevo/e2e_test.go", "w") as f:
    f.write(text)


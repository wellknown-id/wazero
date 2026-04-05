import re

with open("internal/testing/require/require.go", "r") as f:
    r = f.read()
# fix the syntax error (empty import block)
r = re.sub(r'import \(\n*\)', '', r)
with open("internal/testing/require/require.go", "w") as f:
    f.write(r)

with open("internal/platform/time.go", "r") as f:
    t = f.read()

t = re.sub(r'\t"github\.com/tetratelabs/wazero/sys"\n', '', t)
t = re.sub(r' sys\.Walltime', ' func() (sec int64, nsec int32)', t)
t = re.sub(r' sys\.Nanotime', ' func() int64', t)
t = re.sub(r'var FakeNanosleep[^\n]+\n', '', t)
t = re.sub(r'var FakeOsyield[^\n]+\n', '', t)

with open("internal/platform/time.go", "w") as f:
    f.write(t)

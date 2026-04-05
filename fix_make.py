import re

with open("Makefile", "r") as f:
    text = f.read()

# Replace build targets
text = re.sub(r'build/wazero_%/wazero:\n\t\$\(call go-build,\$@,\$<\)\n\nbuild/wazero_%/wazero\.exe:\n\t\$\(call go-build,\$@,\$<\)', 
              r'build/wazero_%/libwazero.a:\n\t$(call go-build,$@,$<)', text)

text = re.sub(r'dist/wazero_\$\(VERSION\)_%\.tar\.gz: build/wazero_%/wazero', 
              r'dist/wazero_$(VERSION)_%.tar.gz: build/wazero_%/libwazero.a', text)

text = re.sub(r'dist/wazero_\$\(VERSION\)_%\.zip: build/wazero_%/wazero\.exe', 
              r'dist/wazero_$(VERSION)_%.zip: build/wazero_%/libwazero.a', text)

# Modify the go-build define
go_build_old = r'-o \$1 \$2 ./cmd/wazero'
go_build_new = r'-buildmode=archive -o $1 .'
text = text.replace(go_build_old, go_build_new)

with open("Makefile", "w") as f:
    f.write(text)


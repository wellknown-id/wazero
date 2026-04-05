with open("internal/testing/require/require.go", "r") as f:
    text = f.read()

text = text.replace('import (\n\t"bytes"\n\t"errors"\n\t"fmt"\n\t"path"\n\t"reflect"\n\t"runtime"\n\t"strings"\n\t"unicode"\n\t"unicode/utf8"\n\n\n\n// TestingT', 'import (\n\t"bytes"\n\t"errors"\n\t"fmt"\n\t"path"\n\t"reflect"\n\t"runtime"\n\t"strings"\n\t"unicode"\n\t"unicode/utf8"\n)\n\n// TestingT')

with open("internal/testing/require/require.go", "w") as f:
    f.write(text)

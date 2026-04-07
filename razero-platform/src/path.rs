#[cfg(windows)]
pub fn to_posix_path(input: &str) -> String {
    input.replace('\\', "/")
}

#[cfg(not(windows))]
pub fn to_posix_path(input: &str) -> String {
    input.to_string()
}

#[cfg(test)]
mod tests {
    use super::to_posix_path;

    #[test]
    fn to_posix_path_matches_go() {
        let mut tests = vec![
            ("", ""),
            (".", "."),
            ("/", "/"),
            ("/foo/bar", "/foo/bar"),
            ("foo/bar", "foo/bar"),
            ("c:/foo/bar", "c:/foo/bar"),
        ];

        if cfg!(windows) {
            tests.extend([
                (r"\foo\bar", "/foo/bar"),
                (r"foo\bar", "foo/bar"),
                (r"c:\foo\bar", "c:/foo/bar"),
            ]);
        }

        for (input, expected) in tests {
            assert_eq!(expected, to_posix_path(input));
        }
    }
}

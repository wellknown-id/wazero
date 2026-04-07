use std::io::{self, Read};

const SEED: u64 = 42;

pub fn new_fake_rand_source() -> FakeRandSource {
    FakeRandSource { state: SEED }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FakeRandSource {
    state: u64,
}

impl FakeRandSource {
    fn next_u64(&mut self) -> u64 {
        // Deterministic pseudo-random stream for test helpers.
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        self.state
    }
}

impl Read for FakeRandSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut filled = 0;
        while filled < buf.len() {
            let bytes = self.next_u64().to_le_bytes();
            let take = (buf.len() - filled).min(bytes.len());
            buf[filled..filled + take].copy_from_slice(&bytes[..take]);
            filled += take;
        }
        Ok(buf.len())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::new_fake_rand_source;

    #[test]
    fn fake_rand_source_is_deterministic() {
        let mut left = new_fake_rand_source();
        let mut right = new_fake_rand_source();
        let mut left_bytes = [0_u8; 32];
        let mut right_bytes = [0_u8; 32];

        left.read_exact(&mut left_bytes)
            .expect("read should succeed");
        right
            .read_exact(&mut right_bytes)
            .expect("read should succeed");

        assert_eq!(left_bytes, right_bytes);
    }

    #[test]
    fn fake_rand_source_advances() {
        let mut reader = new_fake_rand_source();
        let mut first = [0_u8; 16];
        let mut second = [0_u8; 16];

        reader.read_exact(&mut first).expect("read should succeed");
        reader.read_exact(&mut second).expect("read should succeed");

        assert_ne!(first, second);
    }
}

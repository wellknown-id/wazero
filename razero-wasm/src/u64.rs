pub fn le_bytes(value: u64) -> [u8; 8] {
    value.to_le_bytes()
}

pub fn from_le_bytes(bytes: [u8; 8]) -> u64 {
    u64::from_le_bytes(bytes)
}

pub fn rotl(value: u64, amount: u32) -> u64 {
    value.rotate_left(amount)
}

pub fn rotr(value: u64, amount: u32) -> u64 {
    value.rotate_right(amount)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_bytes_match_little_endian_layout() {
        for input in [0, u32::MAX as u64, u64::MAX] {
            assert_eq!(le_bytes(input), input.to_le_bytes());
            assert_eq!(from_le_bytes(le_bytes(input)), input);
        }
    }

    #[test]
    fn rotates_match_intrinsics() {
        let value = 0x1234_5678_9abc_def0;
        assert_eq!(rotl(value, 4), value.rotate_left(4));
        assert_eq!(rotr(value, 4), value.rotate_right(4));
    }
}

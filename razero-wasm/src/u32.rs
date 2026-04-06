pub fn le_bytes(value: u32) -> [u8; 4] {
    value.to_le_bytes()
}

pub fn from_le_bytes(bytes: [u8; 4]) -> u32 {
    u32::from_le_bytes(bytes)
}

pub fn rotl(value: u32, amount: u32) -> u32 {
    value.rotate_left(amount)
}

pub fn rotr(value: u32, amount: u32) -> u32 {
    value.rotate_right(amount)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_bytes_match_little_endian_layout() {
        for input in [0, i32::MAX as u32, u32::MAX] {
            assert_eq!(le_bytes(input), input.to_le_bytes());
            assert_eq!(from_le_bytes(le_bytes(input)), input);
        }
    }

    #[test]
    fn rotates_match_intrinsics() {
        let value = 0x1234_5678;
        assert_eq!(rotl(value, 4), value.rotate_left(4));
        assert_eq!(rotr(value, 4), value.rotate_right(4));
    }
}

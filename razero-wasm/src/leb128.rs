use std::error::Error;
use std::fmt::{Display, Formatter};

const MAX_VARINT_LEN32: usize = 5;
const MAX_VARINT_LEN33: usize = MAX_VARINT_LEN32;
const MAX_VARINT_LEN64: usize = 10;

const CONTINUATION: u8 = 0x80;
const PAYLOAD: u8 = 0x7f;
const SIGN: u8 = 0x40;

const INT33_MASK: i64 = 1 << 7;
const INT33_MASK2: i64 = !INT33_MASK;
const INT33_MASK3: i64 = 1 << 6;
const INT33_MASK4: i64 = 8_589_934_591;
const INT33_MASK5: i64 = 1 << 32;
const INT33_MASK6: i64 = INT33_MASK4 + 1;

const INT64_MASK3: u8 = 1 << 6;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Leb128Error {
    UnexpectedEof,
    Overflow32,
    Overflow33,
    Overflow64,
}

impl Display for Leb128Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnexpectedEof => f.write_str("unexpected end of LEB128 input"),
            Self::Overflow32 => f.write_str("overflows a 32-bit integer"),
            Self::Overflow33 => f.write_str("overflows a 33-bit integer"),
            Self::Overflow64 => f.write_str("overflows a 64-bit integer"),
        }
    }
}

impl Error for Leb128Error {}

pub fn encode_i32(value: i32) -> Vec<u8> {
    encode_i64(i64::from(value))
}

pub fn encode_i64(mut value: i64) -> Vec<u8> {
    let mut buf = Vec::new();

    loop {
        let mut byte = (value as u8) & PAYLOAD;
        let sign = byte & SIGN;
        value >>= 7;

        if (value != -1 || sign == 0) && (value != 0 || sign != 0) {
            byte |= CONTINUATION;
        }

        buf.push(byte);
        if byte & CONTINUATION == 0 {
            break;
        }
    }

    buf
}

pub fn encode_u32(value: u32) -> Vec<u8> {
    encode_u64(u64::from(value))
}

pub fn encode_u64(mut value: u64) -> Vec<u8> {
    let mut buf = Vec::new();

    loop {
        let mut byte = (value as u8) & PAYLOAD;
        value >>= 7;

        if value != 0 {
            byte |= CONTINUATION;
        }

        buf.push(byte);
        if byte & CONTINUATION == 0 {
            return buf;
        }
    }
}

pub fn decode_u32(bytes: &[u8]) -> Result<(u32, usize), Leb128Error> {
    load_u32(bytes)
}

pub fn load_u32(bytes: &[u8]) -> Result<(u32, usize), Leb128Error> {
    let mut ret = 0_u32;
    let mut shift = 0_u32;

    for i in 0..MAX_VARINT_LEN32 {
        let Some(&byte) = bytes.get(i) else {
            return Err(Leb128Error::UnexpectedEof);
        };

        if byte < CONTINUATION {
            if i == MAX_VARINT_LEN32 - 1 && (byte & 0xf0) > 0 {
                return Err(Leb128Error::Overflow32);
            }
            return Ok((ret | (u32::from(byte) << shift), i + 1));
        }

        ret |= u32::from(byte & PAYLOAD) << shift;
        shift += 7;
    }

    Err(Leb128Error::Overflow32)
}

pub fn decode_u64(bytes: &[u8]) -> Result<(u64, usize), Leb128Error> {
    load_u64(bytes)
}

pub fn load_u64(bytes: &[u8]) -> Result<(u64, usize), Leb128Error> {
    if bytes.is_empty() {
        return Err(Leb128Error::UnexpectedEof);
    }

    let mut ret = 0_u64;
    let mut shift = 0_u64;

    for i in 0..MAX_VARINT_LEN64 {
        let Some(&byte) = bytes.get(i) else {
            return Err(Leb128Error::UnexpectedEof);
        };

        if byte < CONTINUATION {
            if i == MAX_VARINT_LEN64 - 1 && byte > 1 {
                return Err(Leb128Error::Overflow64);
            }
            return Ok((ret | (u64::from(byte) << shift), i + 1));
        }

        ret |= u64::from(byte & PAYLOAD) << shift;
        shift += 7;
    }

    Err(Leb128Error::Overflow64)
}

pub fn decode_i32(bytes: &[u8]) -> Result<(i32, usize), Leb128Error> {
    load_i32(bytes)
}

pub fn load_i32(bytes: &[u8]) -> Result<(i32, usize), Leb128Error> {
    let mut ret = 0_u32;
    let mut shift = 0_u32;
    let mut bytes_read = 0_usize;

    loop {
        let Some(&byte) = bytes.get(bytes_read) else {
            return Err(Leb128Error::UnexpectedEof);
        };
        if shift >= 32 {
            return Err(Leb128Error::Overflow32);
        }

        ret |= u32::from(byte & PAYLOAD) << shift;
        shift += 7;
        bytes_read += 1;

        if byte & CONTINUATION == 0 {
            if shift < 32 && (byte & SIGN) != 0 {
                ret |= (!0_u32) << shift;
            }

            let signed_ret = ret as i32;

            if bytes_read > MAX_VARINT_LEN32 {
                return Err(Leb128Error::Overflow32);
            } else if bytes_read == MAX_VARINT_LEN32 {
                let unused = byte & 0b0011_0000;
                if (signed_ret < 0 && unused != 0b0011_0000) || (signed_ret >= 0 && unused != 0) {
                    return Err(Leb128Error::Overflow32);
                }
            }

            return Ok((signed_ret, bytes_read));
        }
    }
}

pub fn decode_i33_as_i64(bytes: &[u8]) -> Result<(i64, usize), Leb128Error> {
    let mut ret = 0_i64;
    let mut shift = 0_u32;
    let mut bytes_read = 0_usize;
    let mut last = 0_i64;

    while shift < 35 {
        let Some(&raw_byte) = bytes.get(bytes_read) else {
            return Err(Leb128Error::UnexpectedEof);
        };

        last = i64::from(raw_byte);
        ret |= (last & INT33_MASK2) << shift;
        shift += 7;
        bytes_read += 1;

        if last & INT33_MASK == 0 {
            break;
        }
    }

    if shift < 33 && (last & INT33_MASK3) == INT33_MASK3 {
        ret |= INT33_MASK4 << shift;
    }
    ret &= INT33_MASK4;

    if ret & INT33_MASK5 > 0 {
        ret -= INT33_MASK6;
    }

    if bytes_read > MAX_VARINT_LEN33 {
        return Err(Leb128Error::Overflow33);
    } else if bytes_read == MAX_VARINT_LEN33 {
        let unused = (last as u8) & 0b0010_0000;
        if (ret < 0 && unused != 0b0010_0000) || (ret >= 0 && unused != 0) {
            return Err(Leb128Error::Overflow33);
        }
    }

    Ok((ret, bytes_read))
}

pub fn decode_i64(bytes: &[u8]) -> Result<(i64, usize), Leb128Error> {
    load_i64(bytes)
}

pub fn load_i64(bytes: &[u8]) -> Result<(i64, usize), Leb128Error> {
    let mut ret = 0_u64;
    let mut shift = 0_u32;
    let mut bytes_read = 0_usize;

    loop {
        let Some(&byte) = bytes.get(bytes_read) else {
            return Err(Leb128Error::UnexpectedEof);
        };
        if shift >= 64 {
            return Err(Leb128Error::Overflow64);
        }

        ret |= u64::from(byte & PAYLOAD) << shift;
        shift += 7;
        bytes_read += 1;

        if byte & CONTINUATION == 0 {
            if shift < 64 && (byte & INT64_MASK3) == INT64_MASK3 {
                ret |= (!0_u64) << shift;
            }

            let signed_ret = ret as i64;

            if bytes_read > MAX_VARINT_LEN64 {
                return Err(Leb128Error::Overflow64);
            } else if bytes_read == MAX_VARINT_LEN64 {
                let unused = byte & 0b0011_1110;
                if (signed_ret < 0 && unused != 0b0011_1110) || (signed_ret >= 0 && unused != 0) {
                    return Err(Leb128Error::Overflow64);
                }
            }

            return Ok((signed_ret, bytes_read));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_i32() {
        for (input, expected) in [
            (-165_675_008, vec![0x80, 0x80, 0x80, 0xb1, 0x7f]),
            (-624_485, vec![0x9b, 0xf1, 0x59]),
            (-16_256, vec![0x80, 0x81, 0x7f]),
            (-4, vec![0x7c]),
            (-1, vec![0x7f]),
            (0, vec![0x00]),
            (1, vec![0x01]),
            (4, vec![0x04]),
            (16_256, vec![0x80, 0xff, 0x00]),
            (624_485, vec![0xe5, 0x8e, 0x26]),
            (165_675_008, vec![0x80, 0x80, 0x80, 0xcf, 0x00]),
            (i32::MAX, vec![0xff, 0xff, 0xff, 0xff, 0x07]),
        ] {
            assert_eq!(expected, encode_i32(input));
            assert_eq!(Ok((input, expected.len())), load_i32(&expected));
        }
    }

    #[test]
    fn encode_decode_i64() {
        for (input, expected) in [
            (-(i64::from(i32::MAX)), vec![0x81, 0x80, 0x80, 0x80, 0x78]),
            (-165_675_008, vec![0x80, 0x80, 0x80, 0xb1, 0x7f]),
            (-624_485, vec![0x9b, 0xf1, 0x59]),
            (-16_256, vec![0x80, 0x81, 0x7f]),
            (-4, vec![0x7c]),
            (-1, vec![0x7f]),
            (0, vec![0x00]),
            (1, vec![0x01]),
            (4, vec![0x04]),
            (16_256, vec![0x80, 0xff, 0x00]),
            (624_485, vec![0xe5, 0x8e, 0x26]),
            (165_675_008, vec![0x80, 0x80, 0x80, 0xcf, 0x00]),
            (i64::from(i32::MAX), vec![0xff, 0xff, 0xff, 0xff, 0x07]),
            (
                i64::MAX,
                vec![0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00],
            ),
        ] {
            assert_eq!(expected, encode_i64(input));
            assert_eq!(Ok((input, expected.len())), load_i64(&expected));
        }
    }

    #[test]
    fn encode_u32_matches_go_cases() {
        for (input, expected) in [
            (0_u32, vec![0x00]),
            (1, vec![0x01]),
            (4, vec![0x04]),
            (16_256, vec![0x80, 0x7f]),
            (624_485, vec![0xe5, 0x8e, 0x26]),
            (165_675_008, vec![0x80, 0x80, 0x80, 0x4f]),
            (u32::MAX, vec![0xff, 0xff, 0xff, 0xff, 0x0f]),
        ] {
            assert_eq!(expected, encode_u32(input));
        }
    }

    #[test]
    fn encode_u64_matches_go_cases() {
        for (input, expected) in [
            (0_u64, vec![0x00]),
            (1, vec![0x01]),
            (4, vec![0x04]),
            (16_256, vec![0x80, 0x7f]),
            (624_485, vec![0xe5, 0x8e, 0x26]),
            (165_675_008, vec![0x80, 0x80, 0x80, 0x4f]),
            (u64::from(u32::MAX), vec![0xff, 0xff, 0xff, 0xff, 0x0f]),
            (
                u64::MAX,
                vec![0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01],
            ),
        ] {
            assert_eq!(expected, encode_u64(input));
        }
    }

    #[test]
    fn decode_u32_matches_go_cases() {
        for (bytes, expected) in [
            (vec![0xff, 0xff, 0xff, 0xff, 0x0f], Ok((u32::MAX, 5))),
            (vec![0x00], Ok((0, 1))),
            (vec![0x04], Ok((4, 1))),
            (vec![0x01], Ok((1, 1))),
            (vec![0x80, 0x00], Ok((0, 2))),
            (vec![0x80, 0x7f], Ok((16_256, 2))),
            (vec![0xe5, 0x8e, 0x26], Ok((624_485, 3))),
            (vec![0x80, 0x80, 0x80, 0x4f], Ok((165_675_008, 4))),
        ] {
            assert_eq!(expected, load_u32(&bytes));
            assert_eq!(expected, decode_u32(&bytes));
        }

        for bytes in [
            vec![0x83, 0x80, 0x80, 0x80, 0x80, 0x00],
            vec![0x82, 0x80, 0x80, 0x80, 0x70],
            vec![0x80, 0x80, 0x80, 0x80, 0x80, 0x00],
        ] {
            assert_eq!(Err(Leb128Error::Overflow32), load_u32(&bytes));
        }
    }

    #[test]
    fn decode_u64_matches_go_cases() {
        for (bytes, expected) in [
            (vec![0x04], Ok((4_u64, 1))),
            (vec![0x80, 0x7f], Ok((16_256, 2))),
            (vec![0xe5, 0x8e, 0x26], Ok((624_485, 3))),
            (vec![0x80, 0x80, 0x80, 0x4f], Ok((165_675_008, 4))),
            (
                vec![0xff, 0xff, 0xff, 0xff, 0x0f],
                Ok((u64::from(u32::MAX), 5)),
            ),
            (
                vec![0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01],
                Ok((u64::MAX, 10)),
            ),
        ] {
            assert_eq!(expected, load_u64(&bytes));
            assert_eq!(expected, decode_u64(&bytes));
        }

        assert_eq!(
            Err(Leb128Error::Overflow64),
            load_u64(&[0x89, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x71])
        );
    }

    #[test]
    fn decode_i32_matches_go_cases() {
        for (bytes, expected) in [
            (vec![0x13], Ok((19_i32, 1))),
            (vec![0x00], Ok((0, 1))),
            (vec![0x04], Ok((4, 1))),
            (vec![0xff, 0x00], Ok((127, 2))),
            (vec![0x81, 0x01], Ok((129, 2))),
            (vec![0x7f], Ok((-1, 1))),
            (vec![0x81, 0x7f], Ok((-127, 2))),
            (vec![0xff, 0x7e], Ok((-129, 2))),
        ] {
            assert_eq!(expected, load_i32(&bytes));
            assert_eq!(expected, decode_i32(&bytes));
        }

        for bytes in [
            vec![0xff, 0xff, 0xff, 0xff, 0x0f],
            vec![0xff, 0xff, 0xff, 0xff, 0x4f],
            vec![0x80, 0x80, 0x80, 0x80, 0x70],
        ] {
            assert_eq!(Err(Leb128Error::Overflow32), load_i32(&bytes));
        }
    }

    #[test]
    fn decode_i33_as_i64_matches_go_cases() {
        for (bytes, expected) in [
            (vec![0x00], Ok((0_i64, 1))),
            (vec![0x04], Ok((4, 1))),
            (vec![0x40], Ok((-64, 1))),
            (vec![0x7f], Ok((-1, 1))),
            (vec![0x7e], Ok((-2, 1))),
            (vec![0x7d], Ok((-3, 1))),
            (vec![0x7c], Ok((-4, 1))),
            (vec![0xff, 0x00], Ok((127, 2))),
            (vec![0x81, 0x01], Ok((129, 2))),
            (vec![0x81, 0x7f], Ok((-127, 2))),
            (vec![0xff, 0x7e], Ok((-129, 2))),
        ] {
            assert_eq!(expected, decode_i33_as_i64(&bytes));
        }
    }

    #[test]
    fn decode_i64_matches_go_cases() {
        for (bytes, expected) in [
            (vec![0x00], Ok((0_i64, 1))),
            (vec![0x04], Ok((4, 1))),
            (vec![0xff, 0x00], Ok((127, 2))),
            (vec![0x81, 0x01], Ok((129, 2))),
            (vec![0x7f], Ok((-1, 1))),
            (vec![0x81, 0x7f], Ok((-127, 2))),
            (vec![0xff, 0x7e], Ok((-129, 2))),
            (
                vec![0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x7f],
                Ok((i64::MIN, 10)),
            ),
        ] {
            assert_eq!(expected, load_i64(&bytes));
            assert_eq!(expected, decode_i64(&bytes));
        }
    }

    #[test]
    fn decode_reports_unexpected_eof() {
        for bytes in [vec![], vec![0x80], vec![0x80, 0x80, 0x80, 0x80]] {
            assert_eq!(Err(Leb128Error::UnexpectedEof), load_u32(&bytes));
            assert_eq!(Err(Leb128Error::UnexpectedEof), load_i32(&bytes));
            assert_eq!(Err(Leb128Error::UnexpectedEof), decode_i33_as_i64(&bytes));
        }

        for bytes in [vec![], vec![0x80], vec![0x80, 0x80, 0x80, 0x80, 0x80]] {
            assert_eq!(Err(Leb128Error::UnexpectedEof), load_u64(&bytes));
            assert_eq!(Err(Leb128Error::UnexpectedEof), load_i64(&bytes));
        }
    }
}

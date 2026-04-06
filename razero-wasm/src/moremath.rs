pub const F32_CANONICAL_NAN_BITS: u32 = crate::ieee754::F32_CANONICAL_NAN_BITS;
pub const F64_CANONICAL_NAN_BITS: u64 = crate::ieee754::F64_CANONICAL_NAN_BITS;
pub const F32_CANONICAL_NAN_BITS_MASK: u32 = 0x7fff_ffff;
pub const F64_CANONICAL_NAN_BITS_MASK: u64 = 0x7fff_ffff_ffff_ffff;
pub const F32_ARITHMETIC_NAN_PAYLOAD_MSB: u32 = 0x0040_0000;
pub const F64_ARITHMETIC_NAN_PAYLOAD_MSB: u64 = 0x0008_0000_0000_0000;
pub const F32_EXPONENT_MASK: u32 = 0x7f80_0000;
pub const F64_EXPONENT_MASK: u64 = 0x7ff0_0000_0000_0000;
pub const F32_ARITHMETIC_NAN_BITS: u32 = F32_CANONICAL_NAN_BITS | 0b1;
pub const F64_ARITHMETIC_NAN_BITS: u64 = F64_CANONICAL_NAN_BITS | 0b1;

pub fn wasm_compat_min32(x: f32, y: f32) -> f32 {
    match () {
        _ if x.is_nan() || y.is_nan() => return_f32_nan_bin_op(x, y),
        _ if (x as f64).is_infinite() && x.is_sign_negative() => f32::NEG_INFINITY,
        _ if (y as f64).is_infinite() && y.is_sign_negative() => f32::NEG_INFINITY,
        _ if x == 0.0 && x == y => {
            if x.is_sign_negative() {
                x
            } else {
                y
            }
        }
        _ if x < y => x,
        _ => y,
    }
}

pub fn wasm_compat_min64(x: f64, y: f64) -> f64 {
    match () {
        _ if x.is_nan() || y.is_nan() => return_f64_nan_bin_op(x, y),
        _ if x.is_infinite() && x.is_sign_negative() => f64::NEG_INFINITY,
        _ if y.is_infinite() && y.is_sign_negative() => f64::NEG_INFINITY,
        _ if x == 0.0 && x == y => {
            if x.is_sign_negative() {
                x
            } else {
                y
            }
        }
        _ if x < y => x,
        _ => y,
    }
}

pub fn wasm_compat_max32(x: f32, y: f32) -> f32 {
    match () {
        _ if x.is_nan() || y.is_nan() => return_f32_nan_bin_op(x, y),
        _ if (x as f64).is_infinite() && x.is_sign_positive() => f32::INFINITY,
        _ if (y as f64).is_infinite() && y.is_sign_positive() => f32::INFINITY,
        _ if x == 0.0 && x == y => {
            if x.is_sign_negative() {
                y
            } else {
                x
            }
        }
        _ if x > y => x,
        _ => y,
    }
}

pub fn wasm_compat_max64(x: f64, y: f64) -> f64 {
    match () {
        _ if x.is_nan() || y.is_nan() => return_f64_nan_bin_op(x, y),
        _ if x.is_infinite() && x.is_sign_positive() => f64::INFINITY,
        _ if y.is_infinite() && y.is_sign_positive() => f64::INFINITY,
        _ if x == 0.0 && x == y => {
            if x.is_sign_negative() {
                y
            } else {
                x
            }
        }
        _ if x > y => x,
        _ => y,
    }
}

pub fn wasm_compat_nearest_f32(value: f32) -> f32 {
    let result = if value != 0.0 {
        let ceil = (value as f64).ceil() as f32;
        let floor = (value as f64).floor() as f32;
        let dist_to_ceil = ((value - ceil) as f64).abs();
        let dist_to_floor = ((value - floor) as f64).abs();
        let half = ceil / 2.0;

        if dist_to_ceil < dist_to_floor {
            ceil
        } else if dist_to_ceil == dist_to_floor && (half as f64).floor() as f32 == half {
            ceil
        } else {
            floor
        }
    } else {
        value
    };

    return_f32_uni_op(value, result)
}

pub fn wasm_compat_nearest_f64(value: f64) -> f64 {
    let result = if value != 0.0 {
        let ceil = value.ceil();
        let floor = value.floor();
        let dist_to_ceil = (value - ceil).abs();
        let dist_to_floor = (value - floor).abs();
        let half = ceil / 2.0;

        if dist_to_ceil < dist_to_floor {
            ceil
        } else if dist_to_ceil == dist_to_floor && half.floor() == half {
            ceil
        } else {
            floor
        }
    } else {
        value
    };

    return_f64_uni_op(value, result)
}

pub fn wasm_compat_ceil_f32(value: f32) -> f32 {
    return_f32_uni_op(value, (value as f64).ceil() as f32)
}

pub fn wasm_compat_ceil_f64(value: f64) -> f64 {
    return_f64_uni_op(value, value.ceil())
}

pub fn wasm_compat_floor_f32(value: f32) -> f32 {
    return_f32_uni_op(value, (value as f64).floor() as f32)
}

pub fn wasm_compat_floor_f64(value: f64) -> f64 {
    return_f64_uni_op(value, value.floor())
}

pub fn wasm_compat_trunc_f32(value: f32) -> f32 {
    return_f32_uni_op(value, (value as f64).trunc() as f32)
}

pub fn wasm_compat_trunc_f64(value: f64) -> f64 {
    return_f64_uni_op(value, value.trunc())
}

pub fn wasm_compat_copysign_f32(value: f32, sign: f32) -> f32 {
    value.copysign(sign)
}

pub fn wasm_compat_copysign_f64(value: f64, sign: f64) -> f64 {
    value.copysign(sign)
}

fn f32_is_nan(value: f32) -> bool {
    value != value
}

fn f64_is_nan(value: f64) -> bool {
    value != value
}

fn return_f32_uni_op(original: f32, result: f32) -> f32 {
    if !f32_is_nan(result) {
        return result;
    }
    if !f32_is_nan(original) {
        return f32::from_bits(F32_CANONICAL_NAN_BITS);
    }
    f32::from_bits(original.to_bits() | F32_CANONICAL_NAN_BITS)
}

fn return_f64_uni_op(original: f64, result: f64) -> f64 {
    if !f64_is_nan(result) {
        return result;
    }
    if !f64_is_nan(original) {
        return f64::from_bits(F64_CANONICAL_NAN_BITS);
    }
    f64::from_bits(original.to_bits() | F64_CANONICAL_NAN_BITS)
}

fn return_f32_nan_bin_op(x: f32, y: f32) -> f32 {
    if f32_is_nan(x) {
        f32::from_bits(x.to_bits() | F32_CANONICAL_NAN_BITS)
    } else {
        f32::from_bits(y.to_bits() | F32_CANONICAL_NAN_BITS)
    }
}

fn return_f64_nan_bin_op(x: f64, y: f64) -> f64 {
    if f64_is_nan(x) {
        f64::from_bits(x.to_bits() | F64_CANONICAL_NAN_BITS)
    } else {
        f64::from_bits(y.to_bits() | F64_CANONICAL_NAN_BITS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_f32_bits_eq(expected: f32, actual: f32) {
        assert_eq!(expected.to_bits(), actual.to_bits());
    }

    fn assert_f64_bits_eq(expected: f64, actual: f64) {
        assert_eq!(expected.to_bits(), actual.to_bits());
    }

    #[test]
    fn wasm_compat_min32_matches_go() {
        assert_eq!(wasm_compat_min32(-1.1, 123.0), -1.1);
        assert_eq!(wasm_compat_min32(-1.1, f32::INFINITY), -1.1);
        assert_eq!(
            wasm_compat_min32(f32::NEG_INFINITY, 123.0),
            f32::NEG_INFINITY
        );

        let canonical = f32::from_bits(F32_CANONICAL_NAN_BITS);
        let arithmetic = f32::from_bits(F32_ARITHMETIC_NAN_BITS);
        assert_f32_bits_eq(canonical, wasm_compat_min32(canonical, canonical));
        assert_f32_bits_eq(canonical, wasm_compat_min32(canonical, arithmetic));
        assert_f32_bits_eq(canonical, wasm_compat_min32(canonical, 1.0));
        assert_f32_bits_eq(arithmetic, wasm_compat_min32(1.0, arithmetic));
        assert_f32_bits_eq(arithmetic, wasm_compat_min32(arithmetic, arithmetic));
    }

    #[test]
    fn wasm_compat_min64_matches_go() {
        assert_eq!(wasm_compat_min64(-1.1, 123.0), -1.1);
        assert_eq!(wasm_compat_min64(-1.1, f64::INFINITY), -1.1);
        assert_eq!(
            wasm_compat_min64(f64::NEG_INFINITY, 123.0),
            f64::NEG_INFINITY
        );

        let canonical = f64::from_bits(F64_CANONICAL_NAN_BITS);
        let arithmetic = f64::from_bits(F64_ARITHMETIC_NAN_BITS);
        assert_f64_bits_eq(canonical, wasm_compat_min64(canonical, canonical));
        assert_f64_bits_eq(canonical, wasm_compat_min64(canonical, arithmetic));
        assert_f64_bits_eq(canonical, wasm_compat_min64(canonical, 1.0));
        assert_f64_bits_eq(arithmetic, wasm_compat_min64(1.0, arithmetic));
        assert_f64_bits_eq(arithmetic, wasm_compat_min64(arithmetic, arithmetic));
    }

    #[test]
    fn wasm_compat_max32_matches_go() {
        assert_eq!(wasm_compat_max32(-1.1, 123.0), 123.0);
        assert_eq!(wasm_compat_max32(-1.1, f32::INFINITY), f32::INFINITY);
        assert_eq!(wasm_compat_max32(f32::NEG_INFINITY, 123.0), 123.0);

        let canonical = f32::from_bits(F32_CANONICAL_NAN_BITS);
        let arithmetic = f32::from_bits(F32_ARITHMETIC_NAN_BITS);
        assert_f32_bits_eq(canonical, wasm_compat_max32(canonical, canonical));
        assert_f32_bits_eq(canonical, wasm_compat_max32(canonical, arithmetic));
        assert_f32_bits_eq(canonical, wasm_compat_max32(canonical, 1.0));
        assert_f32_bits_eq(arithmetic, wasm_compat_max32(1.0, arithmetic));
        assert_f32_bits_eq(arithmetic, wasm_compat_max32(arithmetic, arithmetic));
    }

    #[test]
    fn wasm_compat_max64_matches_go() {
        assert_eq!(wasm_compat_max64(-1.1, 123.1), 123.1);
        assert_eq!(wasm_compat_max64(-1.1, f64::INFINITY), f64::INFINITY);
        assert_eq!(wasm_compat_max64(f64::NEG_INFINITY, 123.1), 123.1);

        let canonical = f64::from_bits(F64_CANONICAL_NAN_BITS);
        let arithmetic = f64::from_bits(F64_ARITHMETIC_NAN_BITS);
        assert_f64_bits_eq(canonical, wasm_compat_max64(canonical, canonical));
        assert_f64_bits_eq(canonical, wasm_compat_max64(canonical, arithmetic));
        assert_f64_bits_eq(canonical, wasm_compat_max64(canonical, 1.0));
        assert_f64_bits_eq(arithmetic, wasm_compat_max64(1.0, arithmetic));
        assert_f64_bits_eq(arithmetic, wasm_compat_max64(arithmetic, arithmetic));
    }

    #[test]
    fn wasm_compat_nearest_preserves_round_to_even_and_zero_sign() {
        assert_eq!(wasm_compat_nearest_f32(-1.5), -2.0);
        assert_eq!(wasm_compat_nearest_f32(-4.5), -4.0);
        assert_eq!((-4.5_f32).round(), -5.0);

        let zero32 = 0.0_f32;
        let neg_zero32 = -zero32;
        assert!(!wasm_compat_nearest_f32(zero32).is_sign_negative());
        assert!(wasm_compat_nearest_f32(neg_zero32).is_sign_negative());

        assert_eq!(wasm_compat_nearest_f64(-1.5), -2.0);
        assert_eq!(wasm_compat_nearest_f64(-4.5), -4.0);
        assert_eq!((-4.5_f64).round(), -5.0);

        let zero64 = 0.0_f64;
        let neg_zero64 = -zero64;
        assert!(!wasm_compat_nearest_f64(zero64).is_sign_negative());
        assert!(wasm_compat_nearest_f64(neg_zero64).is_sign_negative());
    }

    #[test]
    fn unary_ops_propagate_nan_bits() {
        let canonical32 = f32::from_bits(F32_CANONICAL_NAN_BITS);
        let arithmetic32 = f32::from_bits(F32_ARITHMETIC_NAN_BITS);
        let canonical64 = f64::from_bits(F64_CANONICAL_NAN_BITS);
        let arithmetic64 = f64::from_bits(F64_ARITHMETIC_NAN_BITS);

        let f32_ops: [fn(f32) -> f32; 4] = [
            wasm_compat_trunc_f32,
            wasm_compat_nearest_f32,
            wasm_compat_ceil_f32,
            wasm_compat_floor_f32,
        ];
        for op in f32_ops {
            assert_f32_bits_eq(canonical32, op(canonical32));
            assert_f32_bits_eq(arithmetic32, op(arithmetic32));
        }

        let f64_ops: [fn(f64) -> f64; 4] = [
            wasm_compat_trunc_f64,
            wasm_compat_nearest_f64,
            wasm_compat_ceil_f64,
            wasm_compat_floor_f64,
        ];
        for op in f64_ops {
            assert_f64_bits_eq(canonical64, op(canonical64));
            assert_f64_bits_eq(arithmetic64, op(arithmetic64));
        }
    }

    #[test]
    fn return_f32_uni_op_matches_go() {
        let cases = [
            (0_u32, 1.1_f32.to_bits(), 1.1_f32.to_bits()),
            (
                1.0_f32.to_bits(),
                f32::NAN.to_bits(),
                F32_CANONICAL_NAN_BITS,
            ),
            (
                F32_ARITHMETIC_NAN_BITS,
                f32::NAN.to_bits(),
                F32_ARITHMETIC_NAN_BITS,
            ),
            (
                F32_ARITHMETIC_NAN_BITS ^ (1 << 22),
                f32::NAN.to_bits(),
                F32_ARITHMETIC_NAN_BITS,
            ),
        ];

        for (original, result, expected) in cases {
            let actual = return_f32_uni_op(f32::from_bits(original), f32::from_bits(result));
            assert_eq!(actual.to_bits(), expected);
        }
    }

    #[test]
    fn return_f64_uni_op_matches_go() {
        let cases = [
            (0_u64, 1.1_f64.to_bits(), 1.1_f64.to_bits()),
            (
                1.0_f64.to_bits(),
                f64::NAN.to_bits(),
                F64_CANONICAL_NAN_BITS,
            ),
            (
                F64_ARITHMETIC_NAN_BITS,
                f64::NAN.to_bits(),
                F64_ARITHMETIC_NAN_BITS,
            ),
            (
                F64_ARITHMETIC_NAN_BITS ^ (1 << 51),
                f64::NAN.to_bits(),
                F64_ARITHMETIC_NAN_BITS,
            ),
        ];

        for (original, result, expected) in cases {
            let actual = return_f64_uni_op(f64::from_bits(original), f64::from_bits(result));
            assert_eq!(actual.to_bits(), expected);
        }
    }

    #[test]
    fn return_f32_nan_bin_op_matches_go() {
        let cases = [
            (
                F32_CANONICAL_NAN_BITS,
                F32_CANONICAL_NAN_BITS,
                F32_CANONICAL_NAN_BITS,
            ),
            (F32_CANONICAL_NAN_BITS, 0, F32_CANONICAL_NAN_BITS),
            (0, F32_CANONICAL_NAN_BITS, F32_CANONICAL_NAN_BITS),
            (
                F32_ARITHMETIC_NAN_BITS,
                F32_ARITHMETIC_NAN_BITS,
                F32_ARITHMETIC_NAN_BITS,
            ),
            (F32_ARITHMETIC_NAN_BITS, 0, F32_ARITHMETIC_NAN_BITS),
            (0, F32_ARITHMETIC_NAN_BITS, F32_ARITHMETIC_NAN_BITS),
            (
                0,
                F32_ARITHMETIC_NAN_BITS ^ (1 << 22),
                F32_ARITHMETIC_NAN_BITS,
            ),
            (
                F32_ARITHMETIC_NAN_BITS ^ (1 << 22),
                0,
                F32_ARITHMETIC_NAN_BITS,
            ),
        ];

        for (x, y, expected) in cases {
            let actual = return_f32_nan_bin_op(f32::from_bits(x), f32::from_bits(y));
            assert_eq!(actual.to_bits(), expected);
        }
    }

    #[test]
    fn return_f64_nan_bin_op_matches_go() {
        let cases = [
            (
                F64_CANONICAL_NAN_BITS,
                F64_CANONICAL_NAN_BITS,
                F64_CANONICAL_NAN_BITS,
            ),
            (F64_CANONICAL_NAN_BITS, 0, F64_CANONICAL_NAN_BITS),
            (0, F64_CANONICAL_NAN_BITS, F64_CANONICAL_NAN_BITS),
            (
                F64_ARITHMETIC_NAN_BITS,
                F64_ARITHMETIC_NAN_BITS,
                F64_ARITHMETIC_NAN_BITS,
            ),
            (F64_ARITHMETIC_NAN_BITS, 0, F64_ARITHMETIC_NAN_BITS),
            (0, F64_ARITHMETIC_NAN_BITS, F64_ARITHMETIC_NAN_BITS),
            (
                0,
                F64_ARITHMETIC_NAN_BITS ^ (1 << 51),
                F64_ARITHMETIC_NAN_BITS,
            ),
            (
                F64_ARITHMETIC_NAN_BITS ^ (1 << 51),
                0,
                F64_ARITHMETIC_NAN_BITS,
            ),
        ];

        for (x, y, expected) in cases {
            let actual = return_f64_nan_bin_op(f64::from_bits(x), f64::from_bits(y));
            assert_eq!(actual.to_bits(), expected);
        }
    }
}

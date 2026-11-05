//! Port of milo/dtoa_milo.h, the Grisu2 double formatter tippecanoe uses for
//! every number it writes as text. The output has to match the C++ exactly.
//!
//! The C++ picks one of several implementations of `DiyFp::operator*` and
//! `DiyFp::Normalize` depending on the compiler. They all give the same result,
//! so this follows the portable ones.

use std::ops::{Mul, Sub};

#[derive(Clone, Copy, Default)]
struct DiyFp {
    f: u64,
    e: i32,
}

const K_DIY_SIGNIFICAND_SIZE: i32 = 64;
const K_DP_SIGNIFICAND_SIZE: i32 = 52;
const K_DP_EXPONENT_BIAS: i32 = 0x3FF + K_DP_SIGNIFICAND_SIZE;
const K_DP_MIN_EXPONENT: i32 = -K_DP_EXPONENT_BIAS;
const K_DP_EXPONENT_MASK: u64 = 0x7FF00000_00000000;
const K_DP_SIGNIFICAND_MASK: u64 = 0x000FFFFF_FFFFFFFF;
const K_DP_HIDDEN_BIT: u64 = 0x00100000_00000000;

impl DiyFp {
    fn new(ff: u64, ee: i32) -> DiyFp {
        DiyFp { f: ff, e: ee }
    }

    fn from_double(d: f64) -> DiyFp {
        let u64_ = d.to_bits();

        let biased_e = ((u64_ & K_DP_EXPONENT_MASK) >> K_DP_SIGNIFICAND_SIZE) as i32;
        let significand = u64_ & K_DP_SIGNIFICAND_MASK;
        if biased_e != 0 {
            DiyFp {
                f: significand + K_DP_HIDDEN_BIT,
                e: biased_e - K_DP_EXPONENT_BIAS,
            }
        } else {
            DiyFp {
                f: significand,
                e: K_DP_MIN_EXPONENT + 1,
            }
        }
    }

    fn normalize(&self) -> DiyFp {
        let s = self.f.leading_zeros() as i32;
        DiyFp::new(self.f << s, self.e - s)
    }

    fn normalize_boundary(&self) -> DiyFp {
        let mut res = *self;
        while res.f & (K_DP_HIDDEN_BIT << 1) == 0 {
            res.f <<= 1;
            res.e -= 1;
        }
        res.f <<= K_DIY_SIGNIFICAND_SIZE - K_DP_SIGNIFICAND_SIZE - 2;
        res.e -= K_DIY_SIGNIFICAND_SIZE - K_DP_SIGNIFICAND_SIZE - 2;
        res
    }

    /// Returns (minus, plus).
    fn normalized_boundaries(&self) -> (DiyFp, DiyFp) {
        let pl = DiyFp::new((self.f << 1) + 1, self.e - 1).normalize_boundary();
        let mut mi = if self.f == K_DP_HIDDEN_BIT {
            DiyFp::new((self.f << 2) - 1, self.e - 2)
        } else {
            DiyFp::new((self.f << 1) - 1, self.e - 1)
        };
        mi.f <<= mi.e - pl.e;
        mi.e = pl.e;
        (mi, pl)
    }
}

impl Sub for DiyFp {
    type Output = DiyFp;

    fn sub(self, rhs: DiyFp) -> DiyFp {
        DiyFp::new(self.f.wrapping_sub(rhs.f), self.e)
    }
}

impl Mul for DiyFp {
    type Output = DiyFp;

    fn mul(self, rhs: DiyFp) -> DiyFp {
        const M32: u64 = 0xFFFFFFFF;
        let a = self.f >> 32;
        let b = self.f & M32;
        let c = rhs.f >> 32;
        let d = rhs.f & M32;
        let ac = a * c;
        let bc = b * c;
        let ad = a * d;
        let bd = b * d;
        let mut tmp = (bd >> 32) + (ad & M32) + (bc & M32);
        tmp += 1u64 << 31; // mult_round
        DiyFp::new(
            ac + (ad >> 32) + (bc >> 32) + (tmp >> 32),
            self.e + rhs.e + 64,
        )
    }
}

fn get_cached_power(e: i32, k_out: &mut i32) -> DiyFp {
    // 10^-348, 10^-340, ..., 10^340
    static K_CACHED_POWERS_F: [u64; 87] = [
        0xfa8fd5a0_081c0288,
        0xbaaee17f_a23ebf76,
        0x8b16fb20_3055ac76,
        0xcf42894a_5dce35ea,
        0x9a6bb0aa_55653b2d,
        0xe61acf03_3d1a45df,
        0xab70fe17_c79ac6ca,
        0xff77b1fc_bebcdc4f,
        0xbe5691ef_416bd60c,
        0x8dd01fad_907ffc3c,
        0xd3515c28_31559a83,
        0x9d71ac8f_ada6c9b5,
        0xea9c2277_23ee8bcb,
        0xaecc4991_4078536d,
        0x823c1279_5db6ce57,
        0xc2109436_4dfb5637,
        0x9096ea6f_3848984f,
        0xd77485cb_25823ac7,
        0xa086cfcd_97bf97f4,
        0xef340a98_172aace5,
        0xb23867fb_2a35b28e,
        0x84c8d4df_d2c63f3b,
        0xc5dd4427_1ad3cdba,
        0x936b9fce_bb25c996,
        0xdbac6c24_7d62a584,
        0xa3ab6658_0d5fdaf6,
        0xf3e2f893_dec3f126,
        0xb5b5ada8_aaff80b8,
        0x87625f05_6c7c4a8b,
        0xc9bcff60_34c13053,
        0x964e858c_91ba2655,
        0xdff97724_70297ebd,
        0xa6dfbd9f_b8e5b88f,
        0xf8a95fcf_88747d94,
        0xb9447093_8fa89bcf,
        0x8a08f0f8_bf0f156b,
        0xcdb02555_653131b6,
        0x993fe2c6_d07b7fac,
        0xe45c10c4_2a2b3b06,
        0xaa242499_697392d3,
        0xfd87b5f2_8300ca0e,
        0xbce50864_92111aeb,
        0x8cbccc09_6f5088cc,
        0xd1b71758_e219652c,
        0x9c400000_00000000,
        0xe8d4a510_00000000,
        0xad78ebc5_ac620000,
        0x813f3978_f8940984,
        0xc097ce7b_c90715b3,
        0x8f7e32ce_7bea5c70,
        0xd5d238a4_abe98068,
        0x9f4f2726_179a2245,
        0xed63a231_d4c4fb27,
        0xb0de6538_8cc8ada8,
        0x83c7088e_1aab65db,
        0xc45d1df9_42711d9a,
        0x924d692c_a61be758,
        0xda01ee64_1a708dea,
        0xa26da399_9aef774a,
        0xf209787b_b47d6b85,
        0xb454e4a1_79dd1877,
        0x865b8692_5b9bc5c2,
        0xc83553c5_c8965d3d,
        0x952ab45c_fa97a0b3,
        0xde469fbd_99a05fe3,
        0xa59bc234_db398c25,
        0xf6c69a72_a3989f5c,
        0xb7dcbf53_54e9bece,
        0x88fcf317_f22241e2,
        0xcc20ce9b_d35c78a5,
        0x98165af3_7b2153df,
        0xe2a0b5dc_971f303a,
        0xa8d9d153_5ce3b396,
        0xfb9b7cd9_a4a7443c,
        0xbb764c4c_a7a44410,
        0x8bab8eef_b6409c1a,
        0xd01fef10_a657842c,
        0x9b10a4e5_e9913129,
        0xe7109bfb_a19c0c9d,
        0xac2820d9_623bf429,
        0x80444b5e_7aa7cf85,
        0xbf21e440_03acdd2d,
        0x8e679c2f_5e44ff8f,
        0xd433179d_9c8cb841,
        0x9e19db92_b4e31ba9,
        0xeb96bf6e_badf77d9,
        0xaf87023b_9bf0ee6b,
    ];
    #[rustfmt::skip]
    static K_CACHED_POWERS_E: [i16; 87] = [
        -1220, -1193, -1166, -1140, -1113, -1087, -1060, -1034, -1007,  -980,
         -954,  -927,  -901,  -874,  -847,  -821,  -794,  -768,  -741,  -715,
         -688,  -661,  -635,  -608,  -582,  -555,  -529,  -502,  -475,  -449,
         -422,  -396,  -369,  -343,  -316,  -289,  -263,  -236,  -210,  -183,
         -157,  -130,  -103,   -77,   -50,   -24,     3,    30,    56,    83,
          109,   136,   162,   189,   216,   242,   269,   295,   322,   348,
          375,   402,   428,   455,   481,   508,   534,   561,   588,   614,
          641,   667,   694,   720,   747,   774,   800,   827,   853,   880,
          907,   933,   960,   986,  1013,  1039,  1066,
    ];

    //int k = static_cast<int>(ceil((-61 - e) * 0.30102999566398114)) + 374;
    let dk = (-61 - e) as f64 * 0.30102999566398114 + 347.0; // dk must be positive, so can do ceiling in positive
    let mut k = dk as i32;
    if k as f64 != dk {
        k += 1;
    }

    let index = ((k >> 3) + 1) as u32;
    *k_out = -(-348 + (index << 3) as i32); // decimal exponent no need lookup table

    DiyFp::new(
        K_CACHED_POWERS_F[index as usize],
        K_CACHED_POWERS_E[index as usize] as i32,
    )
}

fn grisu_round(buffer: &mut [u8], len: i32, delta: u64, mut rest: u64, ten_kappa: u64, wp_w: u64) {
    while rest < wp_w
        && delta.wrapping_sub(rest) >= ten_kappa
        && (rest.wrapping_add(ten_kappa) < wp_w // closer
            ||
            wp_w.wrapping_sub(rest) > rest.wrapping_add(ten_kappa).wrapping_sub(wp_w))
    {
        buffer[(len - 1) as usize] = buffer[(len - 1) as usize].wrapping_sub(1);
        rest = rest.wrapping_add(ten_kappa);
    }
}

fn count_decimal_digit32(n: u32) -> u32 {
    // Simple pure C++ implementation was faster than __builtin_clz version in this situation.
    if n < 10 {
        return 1;
    }
    if n < 100 {
        return 2;
    }
    if n < 1000 {
        return 3;
    }
    if n < 10000 {
        return 4;
    }
    if n < 100000 {
        return 5;
    }
    if n < 1000000 {
        return 6;
    }
    if n < 10000000 {
        return 7;
    }
    if n < 100000000 {
        return 8;
    }
    if n < 1000000000 {
        return 9;
    }
    10
}

fn digit_gen(
    w: &DiyFp,
    mp: &DiyFp,
    mut delta: u64,
    buffer: &mut Vec<u8>,
    len: &mut i32,
    k: &mut i32,
) {
    static K_POW10: [u32; 10] = [
        1, 10, 100, 1000, 10000, 100000, 1000000, 10000000, 100000000, 1000000000,
    ];
    let one = DiyFp::new(1u64 << -mp.e, mp.e);
    let wp_w = *mp - *w;
    let mut p1 = (mp.f >> -one.e) as u32;
    let mut p2 = mp.f & one.f.wrapping_sub(1);
    let mut kappa = count_decimal_digit32(p1) as i32;
    *len = 0;

    while kappa > 0 {
        let d: u32;
        match kappa {
            10 => {
                d = p1 / 1000000000;
                p1 %= 1000000000;
            }
            9 => {
                d = p1 / 100000000;
                p1 %= 100000000;
            }
            8 => {
                d = p1 / 10000000;
                p1 %= 10000000;
            }
            7 => {
                d = p1 / 1000000;
                p1 %= 1000000;
            }
            6 => {
                d = p1 / 100000;
                p1 %= 100000;
            }
            5 => {
                d = p1 / 10000;
                p1 %= 10000;
            }
            4 => {
                d = p1 / 1000;
                p1 %= 1000;
            }
            3 => {
                d = p1 / 100;
                p1 %= 100;
            }
            2 => {
                d = p1 / 10;
                p1 %= 10;
            }
            1 => {
                d = p1;
                p1 = 0;
            }
            _ => unreachable!(),
        }
        if d != 0 || *len != 0 {
            buffer.push(b'0'.wrapping_add(d as u8));
            *len += 1;
        }
        kappa -= 1;
        let tmp = ((p1 as u64) << -one.e).wrapping_add(p2);
        if tmp <= delta {
            *k += kappa;
            grisu_round(
                buffer,
                *len,
                delta,
                tmp,
                (K_POW10[kappa as usize] as u64) << -one.e,
                wp_w.f,
            );
            return;
        }
    }

    // kappa = 0
    loop {
        p2 = p2.wrapping_mul(10);
        delta = delta.wrapping_mul(10);
        let d = (p2 >> -one.e) as u8;
        if d != 0 || *len != 0 {
            buffer.push(b'0'.wrapping_add(d));
            *len += 1;
        }
        p2 &= one.f.wrapping_sub(1);
        kappa -= 1;
        if p2 < delta {
            *k += kappa;
            let index = -kappa;
            grisu_round(
                buffer,
                *len,
                delta,
                p2,
                one.f,
                wp_w.f.wrapping_mul(if index < 9 {
                    K_POW10[(-kappa) as usize] as u64
                } else {
                    0
                }),
            );
            return;
        }
    }
}

fn grisu2(value: f64, buffer: &mut Vec<u8>, length: &mut i32, k: &mut i32) {
    let v = DiyFp::from_double(value);
    let (w_m, w_p) = v.normalized_boundaries();

    let c_mk = get_cached_power(w_p.e, k);
    let w = v.normalize() * c_mk;
    let mut wp = w_p * c_mk;
    let mut wm = w_m * c_mk;
    wm.f = wm.f.wrapping_add(1);
    wp.f = wp.f.wrapping_sub(1);
    digit_gen(&w, &wp, wp.f.wrapping_sub(wm.f), buffer, length, k);
}

fn get_digits_lut() -> &'static [u8; 200] {
    static C_DIGITS_LUT: [u8; 200] = *b"\
        00010203040506070809\
        10111213141516171819\
        20212223242526272829\
        30313233343536373839\
        40414243444546474849\
        50515253545556575859\
        60616263646566676869\
        70717273747576777879\
        80818283848586878889\
        90919293949596979899";
    &C_DIGITS_LUT
}

fn write_exponent(mut k: i32, buffer: &mut Vec<u8>) {
    if k < 0 {
        buffer.push(b'-');
        k = -k;
    } else {
        buffer.push(b'+');
    }

    if k >= 100 {
        buffer.push(b'0' + (k / 100) as u8);
        k %= 100;
        let d = &get_digits_lut()[(k * 2) as usize..];
        buffer.push(d[0]);
        buffer.push(d[1]);
    } else if k >= 10 {
        let d = &get_digits_lut()[(k * 2) as usize..];
        buffer.push(d[0]);
        buffer.push(d[1]);
    } else {
        buffer.push(b'0' + k as u8);
    }
}

fn prettify(buffer: &mut Vec<u8>, length: i32, k: i32) {
    let kk = length + k; // 10^(kk-1) <= v < 10^kk

    if length <= kk && kk <= 21 {
        // 1234e7 -> 12340000000
        for _ in length..kk {
            buffer.push(b'0');
        }
    } else if 0 < kk && kk <= 21 {
        // 1234e-2 -> 12.34
        buffer.insert(kk as usize, b'.');
    } else if -6 < kk && kk <= 0 {
        // 1234e-6 -> 0.001234
        let offset = 2 - kk;
        buffer.insert(0, b'0');
        buffer.insert(1, b'.');
        for _ in 2..offset {
            buffer.insert(2, b'0');
        }
    } else if length == 1 {
        // 1e30
        buffer.push(b'e');
        write_exponent(kk - 1, buffer);
    } else {
        // 1234e30 -> 1.234e33
        buffer.insert(1, b'.');
        buffer.push(b'e');
        write_exponent(kk - 1, buffer);
    }
}

/// `milo::dtoa_milo`.
pub fn dtoa_milo(mut value: f64) -> String {
    let mut buffer: Vec<u8> = Vec::new();

    if value.is_nan() {
        return "nan".to_string();
    }
    if value.is_infinite() {
        if value < 0.0 {
            return "-inf".to_string();
        } else {
            return "inf".to_string();
        }
    }

    if value == 0.0 {
        buffer.push(b'0');
    } else {
        let mut minus = false;
        if value < 0.0 {
            minus = true;
            value = -value;
        }
        let mut length = 0;
        let mut k = 0;
        grisu2(value, &mut buffer, &mut length, &mut k);
        prettify(&mut buffer, length, k);
        if minus {
            buffer.insert(0, b'-');
        }
    }

    // Only ASCII digits, signs, '.' and 'e' are ever written.
    String::from_utf8(buffer).expect("dtoa_milo produced ASCII")
}

#[cfg(test)]
mod tests {
    use super::dtoa_milo;

    #[test]
    fn formats() {
        assert_eq!(dtoa_milo(0.0), "0");
        assert_eq!(dtoa_milo(-0.0), "0");
        assert_eq!(dtoa_milo(1.0), "1");
        assert_eq!(dtoa_milo(-1.5), "-1.5");
        assert_eq!(dtoa_milo(0.1), "0.1");
        assert_eq!(dtoa_milo(1e21), "1e+21");
        assert_eq!(dtoa_milo(1e20), "100000000000000000000");
        assert_eq!(dtoa_milo(1.5e-7), "1.5e-7");
        assert_eq!(dtoa_milo(0.000001), "0.000001");
        assert_eq!(dtoa_milo(f64::NAN), "nan");
        assert_eq!(dtoa_milo(f64::INFINITY), "inf");
        assert_eq!(dtoa_milo(f64::NEG_INFINITY), "-inf");
        assert_eq!(dtoa_milo(5e-324), "5e-324");
        assert_eq!(dtoa_milo(1.7976931348623157e308), "1.7976931348623157e+308");
        assert_eq!(dtoa_milo(-122.4194155), "-122.4194155");
        assert_eq!(dtoa_milo(1.2345678901234568e23), "1.2345678901234569e+23");
        // Grisu2 is not always the shortest representation
        assert_eq!(dtoa_milo(0.30000000000000004), "0.30000000000000007");
    }
}

//! Port of `almost_equal.hpp`, the Google Test `FloatingPoint<double>::AlmostEquals`.

// # of bits in a number.
const K_BIT_COUNT: u32 = 64;

// # of fraction bits in a number.
const K_FRACTION_BIT_COUNT: u32 = f64::MANTISSA_DIGITS - 1;

// # of exponent bits in a number.
const K_EXPONENT_BIT_COUNT: u32 = K_BIT_COUNT - 1 - K_FRACTION_BIT_COUNT;

// The mask for the sign bit.
const K_SIGN_BIT_MASK: u64 = 1u64 << (K_BIT_COUNT - 1);

// The mask for the fraction bits.
const K_FRACTION_BIT_MASK: u64 = !0u64 >> (K_EXPONENT_BIT_COUNT + 1);

// The mask for the exponent bits.
const K_EXPONENT_BIT_MASK: u64 = !(K_SIGN_BIT_MASK | K_FRACTION_BIT_MASK);

// How many ULP's (Units in the Last Place) we want to tolerate when
// comparing two numbers.
const K_MAX_ULPS: u64 = 4;

// Returns true iff this is NAN (not a number).
fn is_nan(bits: u64) -> bool {
    // It's a NAN if the exponent bits are all ones and the fraction
    // bits are not entirely zeros.
    (bits & K_EXPONENT_BIT_MASK) == K_EXPONENT_BIT_MASK && (bits & K_FRACTION_BIT_MASK) != 0
}

// Converts an integer from the sign-and-magnitude representation to
// the biased representation.
fn sign_and_magnitude_to_biased(sam: u64) -> u64 {
    if K_SIGN_BIT_MASK & sam != 0 {
        // sam represents a negative number.
        (!sam).wrapping_add(1)
    } else {
        // sam represents a positive number.
        K_SIGN_BIT_MASK | sam
    }
}

// Given two numbers in the sign-and-magnitude representation,
// returns the distance between them as an unsigned number.
fn distance_between_sign_and_magnitude_numbers(sam1: u64, sam2: u64) -> u64 {
    let biased1 = sign_and_magnitude_to_biased(sam1);
    let biased2 = sign_and_magnitude_to_biased(sam2);
    if biased1 >= biased2 {
        biased1 - biased2
    } else {
        biased2 - biased1
    }
}

// Returns true iff this number is at most kMaxUlps ULP's away from
// rhs.  In particular, this function:
//
//   - returns false if either number is (or both are) NAN.
//   - treats really large numbers as almost equal to infinity.
//   - thinks +0.0 and -0.0 are 0 DLP's apart.
pub(crate) fn almost_equals(lhs: f64, rhs: f64) -> bool {
    let lhs = lhs.to_bits();
    let rhs = rhs.to_bits();
    // The IEEE standard says that any comparison operation involving
    // a NAN must return false.
    if is_nan(lhs) || is_nan(rhs) {
        return false;
    }
    distance_between_sign_and_magnitude_numbers(lhs, rhs) <= K_MAX_ULPS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulps() {
        assert!(almost_equals(0.0, -0.0));
        assert!(almost_equals(1.0, f64::from_bits(1.0f64.to_bits() + 4)));
        assert!(!almost_equals(1.0, f64::from_bits(1.0f64.to_bits() + 5)));
        assert!(!almost_equals(f64::NAN, f64::NAN));
        assert!(almost_equals(f64::MAX, f64::INFINITY));
        assert!(almost_equals(0.0, f64::from_bits(4)));
        assert!(almost_equals(f64::from_bits(2), -f64::from_bits(2)));
        assert!(!almost_equals(f64::from_bits(3), -f64::from_bits(2)));
    }
}

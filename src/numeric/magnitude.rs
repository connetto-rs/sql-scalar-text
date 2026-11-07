//! Non-negative integers in base 10^9 limbs, the mantissas `numeric` reading and arithmetic work
//! on.

use alloc::{vec, vec::Vec};
use core::cmp::Ordering;

/// The limb base.
const BASE: u64 = 1_000_000_000;

/// A non-negative integer in base 10^9 limbs, least significant first, with no zero limb on top.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Magnitude(Vec<u32>);

impl Magnitude {
    /// The integer whose decimal digits, most significant first, are `digits` followed by `zeros`
    /// zeros.
    pub(super) fn from_digits(digits: &[u8], zeros: usize) -> Self {
        let mut limbs = vec![0; zeros / 9];
        let mut filled = zeros % 9;
        let mut scale = 10u32.pow(u32::try_from(filled).expect("fewer than nine zeros"));
        let mut limb = 0;
        for &digit in digits.iter().rev() {
            limb += u32::from(digit) * scale;
            filled += 1;
            if filled == 9 {
                limbs.push(limb);
                (limb, filled, scale) = (0, 0, 1);
            } else {
                scale *= 10;
            }
        }
        limbs.push(limb);
        let mut magnitude = Self(limbs);
        magnitude.trim();
        magnitude
    }

    /// Multiply by `base^digits` and add `group`.
    pub(super) fn push_group(&mut self, group: u64, base: u32, digits: u32) {
        let factor = u128::from(base).pow(digits);
        let mut carry = u128::from(group);
        for limb in &mut self.0 {
            let value = u128::from(*limb) * factor + carry;
            *limb = narrow(value % u128::from(BASE));
            carry = value / u128::from(BASE);
        }
        while carry > 0 {
            self.0.push(narrow(carry % u128::from(BASE)));
            carry /= u128::from(BASE);
        }
    }

    /// The decimal digits, most significant first, with leading zeros.
    pub(super) fn decimal_digits(&self) -> Vec<u8> {
        let mut digits = Vec::with_capacity(self.0.len() * 9);
        for &limb in self.0.iter().rev() {
            let mut rest = limb;
            let mut chunk = [0u8; 9];
            for slot in chunk.iter_mut().rev() {
                *slot = u8::try_from(rest % 10).expect("a decimal digit fits u8");
                rest /= 10;
            }
            digits.extend_from_slice(&chunk);
        }
        digits
    }

    /// The power of ten of the leading digit, `-1` for zero.
    pub(super) fn decimal_weight(&self) -> i64 {
        let Some(&top) = self.0.last() else {
            return -1;
        };
        let top_digits = top.checked_ilog10().map_or(0, |log| i64::from(log) + 1);
        let lower = i64::try_from(self.0.len() - 1).expect("a limb count fits i64") * 9;
        lower + top_digits - 1
    }

    pub(super) fn add(&self, other: &Self) -> Self {
        let (long, short) = if self.0.len() >= other.0.len() {
            (&self.0, &other.0)
        } else {
            (&other.0, &self.0)
        };
        let mut sum = Vec::with_capacity(long.len() + 1);
        let mut carry = 0;
        for (index, &limb) in long.iter().enumerate() {
            let value = u64::from(limb) + u64::from(short.get(index).copied().unwrap_or(0)) + carry;
            sum.push(narrow_u64(value % BASE));
            carry = value / BASE;
        }
        sum.push(narrow_u64(carry));
        let mut magnitude = Self(sum);
        magnitude.trim();
        magnitude
    }

    /// `self - other`, for `other` no greater than `self`.
    pub(super) fn sub(&self, other: &Self) -> Self {
        let mut difference = Vec::with_capacity(self.0.len());
        let mut borrow = 0;
        for (index, &limb) in self.0.iter().enumerate() {
            let subtrahend = u64::from(other.0.get(index).copied().unwrap_or(0)) + borrow;
            let minuend = u64::from(limb);
            if minuend >= subtrahend {
                difference.push(narrow_u64(minuend - subtrahend));
                borrow = 0;
            } else {
                difference.push(narrow_u64(minuend + BASE - subtrahend));
                borrow = 1;
            }
        }
        let mut magnitude = Self(difference);
        magnitude.trim();
        magnitude
    }

    pub(super) fn mul(&self, other: &Self) -> Self {
        if self.0.is_empty() || other.0.is_empty() {
            return Self::default();
        }
        let mut product = vec![0u64; self.0.len() + other.0.len()];
        for (i, &left) in self.0.iter().enumerate() {
            let mut carry = 0;
            for (j, &right) in other.0.iter().enumerate() {
                let value = product[i + j] + u64::from(left) * u64::from(right) + carry;
                product[i + j] = value % BASE;
                carry = value / BASE;
            }
            product[i + other.0.len()] = carry;
        }
        let mut magnitude = Self(product.into_iter().map(narrow_u64).collect());
        magnitude.trim();
        magnitude
    }

    /// The quotient truncated toward zero and the remainder of `self / divisor`, for a nonzero
    /// divisor, by Knuth's algorithm D.
    pub(super) fn div_rem(&self, divisor: &Self) -> (Self, Self) {
        assert!(
            !divisor.0.is_empty(),
            "a numeric division checks its divisor"
        );
        if *self < *divisor {
            return (Self::default(), self.clone());
        }
        if let [single] = divisor.0[..] {
            let (quotient, remainder) = self.div_rem_limb(u64::from(single));
            return (quotient, Self::from_limb(remainder));
        }
        let top = u64::from(*divisor.0.last().expect("a divisor has limbs"));
        let factor = BASE / (top + 1);
        let mut dividend = self.scaled(factor);
        dividend.push(0);
        let divisor = divisor.scaled(factor);
        let n = divisor.len();
        let (lead, next) = (divisor[n - 1], divisor[n - 2]);
        let mut quotient = vec![0u32; dividend.len() - n];
        for j in (0..quotient.len()).rev() {
            let numerator = dividend[j + n] * BASE + dividend[j + n - 1];
            let mut estimate = numerator / lead;
            let mut rest = numerator % lead;
            while estimate >= BASE || estimate * next > rest * BASE + dividend[j + n - 2] {
                estimate -= 1;
                rest += lead;
                if rest >= BASE {
                    break;
                }
            }
            let mut carry = 0;
            for (i, &limb) in divisor.iter().enumerate() {
                let taken = estimate * limb + carry;
                carry = taken / BASE;
                let taken = taken % BASE;
                if dividend[i + j] >= taken {
                    dividend[i + j] -= taken;
                } else {
                    dividend[i + j] += BASE - taken;
                    carry += 1;
                }
            }
            if dividend[j + n] >= carry {
                dividend[j + n] -= carry;
            } else {
                // The estimate was one too large, so the divisor is added back, its carry out of
                // the top limb cancelling the borrow.
                estimate -= 1;
                let mut carry = 0;
                for (i, &limb) in divisor.iter().enumerate() {
                    let value = dividend[i + j] + limb + carry;
                    dividend[i + j] = value % BASE;
                    carry = value / BASE;
                }
                dividend[j + n] = 0;
            }
            quotient[j] = narrow_u64(estimate);
        }
        let mut quotient = Self(quotient);
        quotient.trim();
        let mut remainder = Self(dividend[..n].iter().copied().map(narrow_u64).collect());
        remainder.trim();
        let (remainder, _) = remainder.div_rem_limb(factor);
        (quotient, remainder)
    }

    /// The quotient and remainder of a division by one nonzero limb.
    fn div_rem_limb(&self, divisor: u64) -> (Self, u64) {
        let mut quotient = vec![0u32; self.0.len()];
        let mut remainder = 0;
        for (slot, &limb) in quotient.iter_mut().zip(&self.0).rev() {
            let value = remainder * BASE + u64::from(limb);
            *slot = narrow_u64(value / divisor);
            remainder = value % divisor;
        }
        let mut quotient = Self(quotient);
        quotient.trim();
        (quotient, remainder)
    }

    /// The limbs of `self * factor` for a factor below the base, with no trimming.
    fn scaled(&self, factor: u64) -> Vec<u64> {
        let mut carry = 0;
        let mut limbs: Vec<u64> = self
            .0
            .iter()
            .map(|&limb| {
                let value = u64::from(limb) * factor + carry;
                carry = value / BASE;
                value % BASE
            })
            .collect();
        if carry > 0 {
            limbs.push(carry);
        }
        limbs
    }

    fn from_limb(limb: u64) -> Self {
        let mut magnitude = Self(vec![narrow_u64(limb)]);
        magnitude.trim();
        magnitude
    }

    fn trim(&mut self) {
        let kept = self
            .0
            .iter()
            .rposition(|&limb| limb != 0)
            .map_or(0, |top| top + 1);
        self.0.truncate(kept);
    }
}

impl PartialOrd for Magnitude {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Magnitude {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0
            .len()
            .cmp(&other.0.len())
            .then_with(|| self.0.iter().rev().cmp(other.0.iter().rev()))
    }
}

/// A limb, which is below 10^9.
fn narrow(value: u128) -> u32 {
    u32::try_from(value).expect("a limb is below 10^9")
}

/// A limb, which is below 10^9.
fn narrow_u64(value: u64) -> u32 {
    u32::try_from(value).expect("a limb is below 10^9")
}

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("missing 0x prefix in `{0}`")]
    MissingPrefix(String),
    #[error("invalid hex in `{0}`")]
    InvalidHex(String),
    #[error("`{input}` is not {expected} bytes")]
    WrongLength { input: String, expected: usize },
    #[error("`{0}` does not fit in 256 bits")]
    Overflow(String),
    #[error("invalid decimal number `{0}`")]
    InvalidDecimal(String),
}

pub(crate) fn decode_hex(s: &str) -> Result<Vec<u8>, ParseError> {
    let digits = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .ok_or_else(|| ParseError::MissingPrefix(s.into()))?;
    let padded;
    let digits = if digits.len() % 2 == 1 {
        padded = format!("0{digits}");
        padded.as_str()
    } else {
        digits
    };
    digits
        .as_bytes()
        .chunks(2)
        .map(|pair| {
            let hi = (pair[0] as char).to_digit(16);
            let lo = (pair[1] as char).to_digit(16);
            match (hi, lo) {
                (Some(h), Some(l)) => Ok((h * 16 + l) as u8),
                _ => Err(ParseError::InvalidHex(s.into())),
            }
        })
        .collect()
}

pub(crate) fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(2 + bytes.len() * 2);
    out.push_str("0x");
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0xf) as usize] as char);
    }
    out
}

macro_rules! fixed_bytes {
    ($name:ident, $len:expr) => {
        #[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub [u8; $len]);

        impl $name {
            pub const ZERO: Self = Self([0; $len]);

            pub fn is_zero(&self) -> bool {
                self.0 == [0; $len]
            }
        }

        impl FromStr for $name {
            type Err = ParseError;

            fn from_str(s: &str) -> Result<Self, ParseError> {
                let bytes = decode_hex(s)?;
                let array = bytes
                    .try_into()
                    .map_err(|_| ParseError::WrongLength { input: s.into(), expected: $len })?;
                Ok(Self(array))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&encode_hex(&self.0))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(self, f)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&self.to_string())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                s.parse().map_err(de::Error::custom)
            }
        }
    };
}

fixed_bytes!(Address, 20);
fixed_bytes!(B256, 32);

impl B256 {
    /// Storage values are 32-byte words, but geth may print them left-trimmed.
    pub fn from_word(s: &str) -> Result<Self, ParseError> {
        let bytes = decode_hex(s)?;
        if bytes.len() > 32 {
            return Err(ParseError::WrongLength { input: s.into(), expected: 32 });
        }
        let mut out = [0u8; 32];
        out[32 - bytes.len()..].copy_from_slice(&bytes);
        Ok(Self(out))
    }
}

/// keccak256 of empty code.
pub const EMPTY_CODE_HASH: B256 = B256([
    0xc5, 0xd2, 0x46, 0x01, 0x86, 0xf7, 0x23, 0x3c, 0x92, 0x7e, 0x7d, 0xb2, 0xdc, 0xc7, 0x03, 0xc0, 0xe5,
    0x00, 0xb6, 0x53, 0xca, 0x82, 0x27, 0x3b, 0x7b, 0xfa, 0xd8, 0x04, 0x5d, 0x85, 0xa4, 0x70,
]);

/// Contract code as raw bytes.
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct Bytes(pub Vec<u8>);

impl Bytes {
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&encode_hex(&self.0))
    }
}

impl fmt::Debug for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Bytes({} bytes)", self.0.len())
    }
}

impl Serialize for Bytes {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        decode_hex(&s).map(Bytes).map_err(de::Error::custom)
    }
}

/// 256-bit unsigned integer (wei amounts), little-endian 64-bit limbs.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct U256([u64; 4]);

impl U256 {
    pub const ZERO: Self = Self([0; 4]);
    pub const MAX: Self = Self([u64::MAX; 4]);

    pub fn from_u128(v: u128) -> Self {
        Self([v as u64, (v >> 64) as u64, 0, 0])
    }

    pub fn is_zero(&self) -> bool {
        self.0 == [0; 4]
    }

    pub fn from_be_slice(bytes: &[u8]) -> Option<Self> {
        let trimmed = &bytes[bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len())..];
        if trimmed.len() > 32 {
            return None;
        }
        let mut word = [0u8; 32];
        word[32 - trimmed.len()..].copy_from_slice(trimmed);
        let mut limbs = [0u64; 4];
        for (i, limb) in limbs.iter_mut().enumerate() {
            let start = 32 - 8 * (i + 1);
            *limb = u64::from_be_bytes(word[start..start + 8].try_into().expect("8 bytes"));
        }
        Some(Self(limbs))
    }

    pub fn to_be_bytes(self) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, limb) in self.0.iter().enumerate() {
            let start = 32 - 8 * (i + 1);
            out[start..start + 8].copy_from_slice(&limb.to_be_bytes());
        }
        out
    }

    pub fn from_hex(s: &str) -> Result<Self, ParseError> {
        let bytes = decode_hex(s)?;
        Self::from_be_slice(&bytes).ok_or_else(|| ParseError::Overflow(s.into()))
    }

    pub fn from_dec(s: &str) -> Result<Self, ParseError> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ParseError::InvalidDecimal(s.into()));
        }
        let mut out = Self::ZERO;
        for b in s.bytes() {
            out = out
                .checked_mul_small(10)
                .and_then(|v| v.checked_add(Self::from_u128(u128::from(b - b'0'))))
                .ok_or_else(|| ParseError::Overflow(s.into()))?;
        }
        Ok(out)
    }

    pub fn checked_add(self, rhs: Self) -> Option<Self> {
        let mut out = [0u64; 4];
        let mut carry = false;
        for (o, (a, b)) in out.iter_mut().zip(self.0.iter().zip(rhs.0)) {
            let (s1, c1) = a.overflowing_add(b);
            let (s2, c2) = s1.overflowing_add(u64::from(carry));
            *o = s2;
            carry = c1 || c2;
        }
        (!carry).then_some(Self(out))
    }

    pub fn checked_sub(self, rhs: Self) -> Option<Self> {
        let mut out = [0u64; 4];
        let mut borrow = false;
        for (o, (a, b)) in out.iter_mut().zip(self.0.iter().zip(rhs.0)) {
            let (d1, b1) = a.overflowing_sub(b);
            let (d2, b2) = d1.overflowing_sub(u64::from(borrow));
            *o = d2;
            borrow = b1 || b2;
        }
        (!borrow).then_some(Self(out))
    }

    fn checked_mul_small(self, m: u64) -> Option<Self> {
        let mut out = [0u64; 4];
        let mut carry = 0u128;
        for (o, a) in out.iter_mut().zip(self.0) {
            let v = u128::from(a) * u128::from(m) + carry;
            *o = v as u64;
            carry = v >> 64;
        }
        (carry == 0).then_some(Self(out))
    }

    fn div_rem_small(self, d: u64) -> (Self, u64) {
        let mut out = [0u64; 4];
        let mut rem = 0u128;
        for i in (0..4).rev() {
            let v = (rem << 64) | u128::from(self.0[i]);
            out[i] = (v / u128::from(d)) as u64;
            rem = v % u128::from(d);
        }
        (Self(out), rem as u64)
    }

    pub fn to_hex_quantity(self) -> String {
        if self.is_zero() {
            return "0x0".into();
        }
        let hex = encode_hex(&self.to_be_bytes());
        format!("0x{}", hex[2..].trim_start_matches('0'))
    }

    /// Formats a wei amount in ether with up to 18 decimals, trailing zeros removed.
    pub fn format_ether(self) -> String {
        let s = self.to_string();
        let (int, frac) = if s.len() > 18 { s.split_at(s.len() - 18) } else { ("0", s.as_str()) };
        let frac = format!("{frac:0>18}");
        let frac = frac.trim_end_matches('0');
        if frac.is_empty() { int.to_string() } else { format!("{int}.{frac}") }
    }
}

impl Ord for U256 {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.iter().rev().cmp(other.0.iter().rev())
    }
}

impl PartialOrd for U256 {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for U256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_zero() {
            return f.write_str("0");
        }
        let mut digits = Vec::new();
        let mut v = *self;
        while !v.is_zero() {
            let (q, r) = v.div_rem_small(10);
            digits.push(b'0' + r as u8);
            v = q;
        }
        digits.reverse();
        f.write_str(std::str::from_utf8(&digits).expect("ascii"))
    }
}

impl fmt::Debug for U256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for U256 {
    type Err = ParseError;

    /// Accepts `0x` hex quantities and plain decimals.
    fn from_str(s: &str) -> Result<Self, ParseError> {
        if s.starts_with("0x") || s.starts_with("0X") { Self::from_hex(s) } else { Self::from_dec(s) }
    }
}

impl Serialize for U256 {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

/// Accepts hex strings, decimal strings and JSON numbers, since clients differ.
impl<'de> Deserialize<'de> for U256 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = U256;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a hex or decimal quantity")
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<U256, E> {
                Ok(U256::from_u128(u128::from(v)))
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<U256, E> {
                v.parse().map_err(E::custom)
            }
        }
        d.deserialize_any(V)
    }
}

/// Deserializes a `u64` given as a JSON number or a hex/decimal string.
pub(crate) fn de_quantity_u64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    let v = Option::<U256>::deserialize(d)?;
    v.map(|v| {
        let limbs = v.0;
        if limbs[1..] == [0, 0, 0] { Ok(limbs[0]) } else { Err(de::Error::custom("nonce exceeds 64 bits")) }
    })
    .transpose()
}

/// A signed difference between two `U256` values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Delta {
    pub negative: bool,
    pub magnitude: U256,
}

impl Delta {
    pub fn between(from: U256, to: U256) -> Self {
        match to.checked_sub(from) {
            Some(m) => Self { negative: false, magnitude: m },
            None => Self { negative: true, magnitude: from.checked_sub(to).expect("from > to") },
        }
    }

    fn sign(&self) -> &'static str {
        if self.magnitude.is_zero() {
            ""
        } else if self.negative {
            "-"
        } else {
            "+"
        }
    }

    pub fn format_ether(&self) -> String {
        format!("{}{}", self.sign(), self.magnitude.format_ether())
    }
}

impl fmt::Display for Delta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.sign(), self.magnitude)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_round_trip() {
        let a: Address = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8".parse().unwrap();
        assert_eq!(a.to_string(), "0x70997970c51812dc3a010c7d01b50e0d17dc79c8");
        assert!("70997970c51812dc3a010c7d01b50e0d17dc79c8".parse::<Address>().is_err());
        assert!("0x1234".parse::<Address>().is_err());
        assert!("0xzz997970c51812dc3a010c7d01b50e0d17dc79c8".parse::<Address>().is_err());
    }

    #[test]
    fn u256_parsing_and_display() {
        let max = "115792089237316195423570985008687907853269984665640564039457584007913129639935";
        assert_eq!(U256::from_dec(max).unwrap(), U256::MAX);
        assert_eq!(U256::MAX.to_string(), max);
        assert_eq!(U256::from_hex(&U256::MAX.to_hex_quantity()).unwrap(), U256::MAX);
        assert_eq!("0x0".parse::<U256>().unwrap(), U256::ZERO);
        assert_eq!("0xde0b6b3a7640000".parse::<U256>().unwrap().to_string(), "1000000000000000000");
        assert!(U256::from_dec(&format!("{max}0")).is_err());
        assert!(U256::from_hex(&format!("0x1{}", "0".repeat(64))).is_err());
        assert_eq!(U256::ZERO.to_hex_quantity(), "0x0");
        assert_eq!(U256::from_u128(255).to_hex_quantity(), "0xff");
    }

    #[test]
    fn ether_formatting() {
        assert_eq!(U256::from_dec("1000000000000000000").unwrap().format_ether(), "1");
        assert_eq!(U256::from_dec("1500000000000000000").unwrap().format_ether(), "1.5");
        assert_eq!(U256::from_u128(21000).format_ether(), "0.000000000000021");
        assert_eq!(U256::ZERO.format_ether(), "0");
    }

    #[test]
    fn deltas() {
        let a = U256::from_u128(10);
        let b = U256::from_u128(3);
        assert_eq!(Delta::between(a, b).to_string(), "-7");
        assert_eq!(Delta::between(b, a).to_string(), "+7");
        assert_eq!(Delta::between(a, a).to_string(), "0");
        assert!(U256::MAX.checked_add(U256::from_u128(1)).is_none());
        assert!(U256::ZERO.checked_sub(U256::from_u128(1)).is_none());
        assert!(U256::from_u128(1 << 64) > U256::from_u128(u64::MAX as u128));
    }

    #[test]
    fn storage_words() {
        assert_eq!(B256::from_word("0x7").unwrap().0[31], 7);
        assert!(B256::from_word(&format!("0x{}", "11".repeat(33))).is_err());
    }
}

//! Structural/checksum validators shared by `domain::patterns::*`.
//!
//! Kept separate from the patterns that use them so each validator is
//! independently unit-testable against known-good/known-bad values, per
//! dlp-bench's own `deterministic` module split.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

/// Luhn checksum, used to validate candidate credit card numbers.
/// `digits` must contain only ASCII digits (callers pre-filter via regex).
#[must_use]
pub fn luhn_check(digits: &str) -> bool {
    let mut sum = 0u32;
    let mut double = false;
    for ch in digits.chars().rev() {
        let Some(mut d) = ch.to_digit(10) else {
            return false;
        };
        if double {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
        double = !double;
    }
    sum.is_multiple_of(10)
}

/// IBAN mod-97-10 checksum (ISO 7064). `iban` must be uppercase
/// alphanumeric with no spaces (callers normalize before calling) and 15 to
/// 34 characters long, the range ISO 13616 allows.
#[must_use]
pub fn iban_checksum_valid(iban: &str) -> bool {
    if iban.len() < 15 || iban.len() > 34 {
        return false;
    }
    if !iban.chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }

    // Move the first four characters (country code + check digits) to the
    // end, then convert letters to numbers (A=10, B=11, ...).
    let rearranged = format!("{}{}", &iban[4..], &iban[..4]);
    let mut numeric = String::with_capacity(rearranged.len() * 2);
    for ch in rearranged.chars() {
        if ch.is_ascii_digit() {
            numeric.push(ch);
        } else if ch.is_ascii_uppercase() {
            numeric.push_str(&(u32::from(ch as u8 - b'A') + 10).to_string());
        } else {
            return false;
        }
    }

    // mod-97 over a big numeric string, computed digit-by-digit to avoid
    // overflow.
    let mut remainder: u32 = 0;
    for ch in numeric.chars() {
        let digit = ch.to_digit(10).unwrap_or(0);
        remainder = (remainder * 10 + digit) % 97;
    }
    remainder == 1
}

#[must_use]
pub fn is_valid_ipv4(s: &str) -> bool {
    Ipv4Addr::from_str(s).is_ok()
}

#[must_use]
pub fn is_valid_ipv6(s: &str) -> bool {
    Ipv6Addr::from_str(s).is_ok()
}

/// Shannon entropy in bits/char, used as a gate for generic `key=value`
/// secret detection (a low-entropy value like `password=changeme` is not
/// worth flagging; a high-entropy one likely is).
#[must_use]
pub fn shannon_entropy(s: &str) -> f64 {
    if s.is_empty() {
        return 0.0;
    }
    let mut counts: HashMap<char, u32> = HashMap::new();
    for c in s.chars() {
        *counts.entry(c).or_insert(0) += 1;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "char counts of scanned values are far below 2^52; exactness is irrelevant for an entropy heuristic"
    )]
    let len = s.chars().count() as f64;
    counts
        .values()
        .map(|&count| {
            let p = f64::from(count) / len;
            -p * p.log2()
        })
        .sum()
}

/// Structural check for a JWT: three `.`-separated base64url segments where
/// the first (header) segment decodes to something JSON-shaped. Does not
/// validate the signature — this is a shape check, not cryptographic
/// verification.
#[must_use]
pub fn looks_like_jwt(candidate: &str) -> bool {
    let parts: Vec<&str> = candidate.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        return false;
    }
    if !parts.iter().all(|p| {
        p.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    }) {
        return false;
    }
    decode_base64url(parts[0]).is_some_and(|bytes| bytes.first() == Some(&b'{'))
}

/// Minimal base64url decoder (no external dependency needed for this one
/// shape check) — unpadded alphabet per RFC 4648 §5.
fn decode_base64url(s: &str) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    }

    let bytes = s.as_bytes();
    #[expect(
        clippy::integer_division,
        reason = "capacity hint: 3 output bytes per 4 base64 chars, rounding is irrelevant"
    )]
    let capacity = bytes.len() * 3 / 4 + 3;
    let mut out = Vec::with_capacity(capacity);
    let mut buf: u32 = 0;
    let mut bits = 0u32;
    for &b in bytes {
        let v = value(b)?;
        buf = (buf << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xFF) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// Build an IBAN with correct check digits for `country` + `bban`.
    fn with_check_digits(country: &str, bban: &str) -> String {
        let numeric: String = format!("{bban}{country}00")
            .chars()
            .map(|c| c.to_digit(36).expect("alphanumeric").to_string())
            .collect();
        let rem = numeric.chars().fold(0u32, |acc, c| {
            (acc * 10 + c.to_digit(10).expect("digit")) % 97
        });
        format!("{country}{:02}{bban}", 98 - rem)
    }

    #[test]
    fn luhn_check_accepts_known_valid_card() {
        assert!(luhn_check("4111111111111111"));
        assert!(luhn_check("5500005555555559"));
    }

    #[test]
    fn luhn_check_rejects_tampered_card_and_non_digits() {
        assert!(!luhn_check("4111111111111112"));
        assert!(!luhn_check("4111-1111"));
    }

    #[test]
    fn iban_checksum_valid_accepts_known_good_ibans() {
        assert!(iban_checksum_valid("GB82WEST12345698765432"));
        assert!(iban_checksum_valid("DE89370400440532013000"));
        // Shortest (Norway, 15) and a 32-character (Saint Lucia) real IBAN.
        assert!(iban_checksum_valid("NO9386011117947"));
        assert!(iban_checksum_valid("LC55HEMM000100010012001200023015"));
    }

    #[test]
    fn iban_checksum_valid_rejects_tampered_iban() {
        assert!(!iban_checksum_valid("GB82WEST12345698765431"));
    }

    #[test]
    fn iban_length_boundaries_are_15_and_34() {
        let len14 = with_check_digits("NO", "1234567890");
        let len15 = with_check_digits("NO", "12345678901");
        let len34 = with_check_digits("XX", &"1".repeat(30));
        let len35 = with_check_digits("XX", &"1".repeat(31));
        assert_eq!(
            (len14.len(), len15.len(), len34.len(), len35.len()),
            (14, 15, 34, 35)
        );

        assert!(!iban_checksum_valid(&len14), "14 chars is too short");
        assert!(iban_checksum_valid(&len15));
        assert!(iban_checksum_valid(&len34));
        assert!(!iban_checksum_valid(&len35), "35 chars is too long");
    }

    #[test]
    fn iban_rejects_spaces_lowercase_and_non_ascii() {
        assert!(!iban_checksum_valid("DE89 3704 0044 0532 0130 00"));
        assert!(!iban_checksum_valid("gb82west12345698765432"));
        assert!(!iban_checksum_valid("GB82WEST1234569876543\u{e9}"));
    }

    proptest! {
        #[test]
        fn iban_validator_never_panics(s in "\\PC{0,40}") {
            // Any result is fine; reaching here without a panic is the test.
            let _valid: bool = iban_checksum_valid(&s);
        }

        #[test]
        fn generated_ibans_validate_and_a_changed_digit_does_not(
            country in "[A-Z]{2}",
            bban in "[A-Z0-9]{10,29}[0-9]",
            new_digit in 0u32..10,
        ) {
            let iban = with_check_digits(&country, &bban);
            prop_assert!(iban_checksum_valid(&iban), "{}", iban);

            // Mod 97 catches every single-digit substitution.
            let last = iban.chars().last().and_then(|c| c.to_digit(10)).expect("digit");
            prop_assume!(new_digit != last);
            let tampered = format!(
                "{}{}",
                &iban[..iban.len() - 1],
                char::from_digit(new_digit, 10).expect("digit")
            );
            prop_assert!(!iban_checksum_valid(&tampered), "{}", tampered);
        }
    }

    #[test]
    fn ip_validators_accept_valid_addresses() {
        assert!(is_valid_ipv4("192.168.1.1"));
        assert!(!is_valid_ipv4("999.1.1.1"));
        assert!(is_valid_ipv6("2001:db8::1"));
        assert!(is_valid_ipv6("::1"));
        assert!(is_valid_ipv6("::ffff:10.0.0.1"));
        assert!(!is_valid_ipv6("not-an-ip"));
        assert!(!is_valid_ipv6("12:30:45"));
    }

    #[test]
    fn shannon_entropy_has_exact_values_in_bits() {
        // Exact values catch a wrong log base (ln would give 0.693 for "ab").
        let cases = [
            ("", 0.0),
            ("aaaaaaaa", 0.0),
            ("ab", 1.0),
            ("aabb", 1.0),
            ("abcd", 2.0),
            ("abcdefgh", 3.0),
            ("aaab", 0.811_278_124_459_132_8),
            // Counted per char, not per byte: two identical 2-byte chars.
            ("\u{e9}\u{e9}", 0.0),
        ];
        for (s, expected) in cases {
            let got = shannon_entropy(s);
            assert!(
                (got - expected).abs() < 1e-12,
                "{s:?}: expected {expected}, got {got}"
            );
        }
    }

    #[test]
    fn looks_like_jwt_accepts_structurally_valid_token() {
        // header {"alg":"HS256","typ":"JWT"} base64url-encoded, plus two
        // arbitrary non-empty segments.
        let header = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9";
        assert!(looks_like_jwt(&format!("{header}.payload.signature")));
    }

    #[test]
    fn looks_like_jwt_rejects_non_jwt_strings() {
        assert!(!looks_like_jwt("not.a.jwt.token"));
        assert!(!looks_like_jwt("just-one-segment"));
        assert!(!looks_like_jwt("a.b"));
    }

    #[test]
    fn looks_like_jwt_requires_a_json_object_header() {
        // Right shape, but "aGVsbG8gd29ybGQ" decodes to "hello world".
        assert!(!looks_like_jwt("aGVsbG8gd29ybGQ.payload.signature"));
        // `[` is JSON, but not the object a JWT header must be.
        assert!(!looks_like_jwt("WzEsMiwzXQ.payload.signature"));
    }

    #[test]
    fn looks_like_jwt_rejects_bad_segments() {
        let header = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9";
        assert!(
            !looks_like_jwt(&format!("{header}..signature")),
            "empty segment"
        );
        assert!(
            !looks_like_jwt(&format!("{header}.pay+load.signature")),
            "base64 `+`"
        );
        assert!(
            !looks_like_jwt(&format!("{header}=.payload.signature")),
            "padding"
        );
    }

    #[test]
    fn decode_base64url_matches_known_vectors() {
        assert_eq!(decode_base64url("aGVsbG8").as_deref(), Some(&b"hello"[..]));
        assert_eq!(decode_base64url("-_8").as_deref(), Some(&[0xfb, 0xff][..]));
        assert_eq!(decode_base64url("a+b"), None);
    }
}

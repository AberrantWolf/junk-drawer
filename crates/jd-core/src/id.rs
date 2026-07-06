//! ULID note identity, written in-house (spec Appendix B).
//! Entropy is a non-cryptographic xorshift seeded from time/pid/stack address —
//! IDs are collision-resistant identifiers in a single-user app, not security
//! tokens. Documented, accepted trade (architecture doc §6.2).

use std::fmt;

use crate::rng::Xorshift128;
use crate::time::Timestamp;

/// 48-bit big-endian millisecond timestamp + 80 bits of randomness.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct NoteId(pub [u8; 16]);

#[derive(Debug, PartialEq, Eq)]
pub enum IdError {
    Length(usize),
    Char(char),
    Overflow,
}

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

fn decode_char(c: u8) -> Option<u8> {
    match c.to_ascii_uppercase() {
        b'0'..=b'9' => Some(c.to_ascii_uppercase() - b'0'),
        b'A' => Some(10),
        b'B' => Some(11),
        b'C' => Some(12),
        b'D' => Some(13),
        b'E' => Some(14),
        b'F' => Some(15),
        b'G' => Some(16),
        b'H' => Some(17),
        b'J' => Some(18),
        b'K' => Some(19),
        b'M' => Some(20),
        b'N' => Some(21),
        b'P' => Some(22),
        b'Q' => Some(23),
        b'R' => Some(24),
        b'S' => Some(25),
        b'T' => Some(26),
        b'V' => Some(27),
        b'W' => Some(28),
        b'X' => Some(29),
        b'Y' => Some(30),
        b'Z' => Some(31),
        _ => None,
    }
}

impl fmt::Display for NoteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = u128::from_be_bytes(self.0);
        let mut out = [0u8; 26];
        for (i, slot) in out.iter_mut().enumerate() {
            let shift = 5 * (25 - i);
            *slot = ALPHABET[((n >> shift) & 0x1F) as usize];
        }
        f.write_str(std::str::from_utf8(&out).expect("alphabet is ASCII"))
    }
}

impl NoteId {
    pub fn parse(s: &str) -> Result<Self, IdError> {
        let b = s.as_bytes();
        if b.len() != 26 {
            return Err(IdError::Length(b.len()));
        }
        let first = decode_char(b[0]).ok_or(IdError::Char(b[0] as char))?;
        if first > 7 {
            return Err(IdError::Overflow); // 26×5 = 130 bits; top 2 must be zero
        }
        let mut n: u128 = 0;
        for &c in b {
            let v = decode_char(c).ok_or(IdError::Char(c as char))?;
            n = (n << 5) | v as u128;
        }
        Ok(NoteId(n.to_be_bytes()))
    }

    /// First 8 display chars — the filename collision suffix (spec §2).
    pub fn short(&self) -> String {
        self.to_string()[..8].to_owned()
    }

    pub fn timestamp_ms(&self) -> u64 {
        let mut b = [0u8; 8];
        b[2..].copy_from_slice(&self.0[..6]);
        u64::from_be_bytes(b)
    }

    /// Strictly monotonic within a process: same-or-earlier millisecond
    /// increments the previous ID's random part instead (ULID spec behavior).
    pub fn generate(r#gen: &mut IdGen) -> Self {
        let now_ms = Timestamp::now().0.max(0) as u64;
        let id = match r#gen.last {
            Some(last) if last.timestamp_ms() >= now_ms => {
                // +1 on the full 128 bits; carry past the 80-bit random part
                // into the timestamp is astronomically unlikely and harmless.
                NoteId(u128::from_be_bytes(last.0).wrapping_add(1).to_be_bytes())
            }
            _ => {
                let mut bytes = [0u8; 16];
                bytes[..6].copy_from_slice(&now_ms.to_be_bytes()[2..]);
                bytes[6..14].copy_from_slice(&r#gen.rng.next_u64().to_be_bytes());
                bytes[14..16].copy_from_slice(&r#gen.rng.next_u64().to_be_bytes()[..2]);
                NoteId(bytes)
            }
        };
        r#gen.last = Some(id);
        id
    }
}

pub struct IdGen {
    rng: Xorshift128,
    last: Option<NoteId>,
}

impl IdGen {
    pub fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let pid = std::process::id() as u64;
        let stack_entropy = {
            let local = 0u8;
            &local as *const u8 as u64
        };
        IdGen {
            rng: Xorshift128::new(nanos ^ (pid << 32) ^ stack_entropy),
            last: None,
        }
    }
}

impl Default for IdGen {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_and_max_display() {
        assert_eq!(NoteId([0; 16]).to_string(), "00000000000000000000000000");
        assert_eq!(NoteId([0xFF; 16]).to_string(), "7ZZZZZZZZZZZZZZZZZZZZZZZZZ");
    }

    #[test]
    fn display_parse_round_trip() {
        let mut r#gen = IdGen::new();
        for _ in 0..100 {
            let id = NoteId::generate(&mut r#gen);
            let s = id.to_string();
            assert_eq!(s.len(), 26);
            assert_eq!(NoteId::parse(&s).unwrap(), id);
        }
    }

    #[test]
    fn spec_example_parses() {
        // the ULID from the design doc's frontmatter example
        let id = NoteId::parse("01J8ZQ4KF3T9M2X7C5VBNAE8RD").unwrap();
        assert_eq!(id.to_string(), "01J8ZQ4KF3T9M2X7C5VBNAE8RD");
        assert_eq!(id.short(), "01J8ZQ4K");
    }

    #[test]
    fn lowercase_accepted() {
        let a = NoteId::parse("01j8zq4kf3t9m2x7c5vbnae8rd").unwrap();
        let b = NoteId::parse("01J8ZQ4KF3T9M2X7C5VBNAE8RD").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(matches!(NoteId::parse(""), Err(IdError::Length(0))));
        assert!(matches!(NoteId::parse("01J8"), Err(IdError::Length(4))));
        // I, L, O, U are not in the Crockford/ULID alphabet
        assert!(NoteId::parse("0IJ8ZQ4KF3T9M2X7C5VBNAE8RD").is_err());
        assert!(NoteId::parse("0LJ8ZQ4KF3T9M2X7C5VBNAE8RD").is_err());
        assert!(NoteId::parse("0OJ8ZQ4KF3T9M2X7C5VBNAE8RD").is_err());
        assert!(NoteId::parse("0UJ8ZQ4KF3T9M2X7C5VBNAE8RD").is_err());
        // first char > '7' overflows 128 bits
        assert!(matches!(
            NoteId::parse("8ZZZZZZZZZZZZZZZZZZZZZZZZZ"),
            Err(IdError::Overflow)
        ));
    }

    #[test]
    fn generate_embeds_current_time() {
        let mut r#gen = IdGen::new();
        let before = crate::time::Timestamp::now().0 as u64;
        let id = NoteId::generate(&mut r#gen);
        let after = crate::time::Timestamp::now().0 as u64;
        assert!(id.timestamp_ms() >= before && id.timestamp_ms() <= after);
    }

    #[test]
    fn generate_is_monotonic() {
        let mut r#gen = IdGen::new();
        let mut prev = NoteId::generate(&mut r#gen);
        for _ in 0..1000 {
            let next = NoteId::generate(&mut r#gen);
            assert!(
                next > prev,
                "ULIDs must be strictly increasing within a process"
            );
            prev = next;
        }
    }
}

# WP1a — Foundation Types & Parsers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `jd-core`'s foundation — `Timestamp`, `NoteId`/ULID, xorshift RNG, `Tag`, note types, the byte-identity frontmatter parser, `NoteDoc` with body extractors — plus the golden corpus and randomized round-trip tests.

**Architecture:** Pure, dependency-free modules in `jd-core` per `docs/superpowers/plans/2026-07-06-technical-architecture.md` §2.1–2.7. The load-bearing invariant: **parse → serialize is byte-identical** for any file not deliberately changed, achieved by preserving raw lines and rewriting only lines a setter touches.

**Tech Stack:** Rust stable, std only. No new dependencies — `serde`, YAML crates, `chrono`, `rand`, and `ulid` are all explicitly rejected (spec Appendix B).

## Global Constraints

- **Zero new dependencies in `jd-core`.**
- Every commit leaves `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` green.
- Public signatures must match the architecture doc §2; deviations require editing that doc first.
- Tests are table-driven where the input space is enumerable; randomized (fixed-seed xorshift) where it isn't.
- Each task adds its `pub mod` line to `crates/jd-core/src/lib.rs`.

---

### Task 1: `rng.rs` — Xorshift128+

**Files:**
- Create: `crates/jd-core/src/rng.rs`
- Modify: `crates/jd-core/src/lib.rs` (add `pub mod rng;`)

**Interfaces:**
- Consumes: nothing.
- Produces: `Xorshift128::new(seed: u64) -> Self`, `next_u64(&mut self) -> u64`, `gen_range(&mut self, range: Range<u64>) -> u64`. Consumed by Task 3 (`IdGen`) and Task 9 (test generator); later by WP1d's synthetic-vault generator.

- [ ] **Step 1: Write the failing tests** (in `rng.rs`'s `#[cfg(test)]` module)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let mut a = Xorshift128::new(42);
        let mut b = Xorshift128::new(42);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = Xorshift128::new(1);
        let mut b = Xorshift128::new(2);
        let same = (0..100).filter(|_| a.next_u64() == b.next_u64()).count();
        assert!(same < 3);
    }

    #[test]
    fn zero_seed_is_not_degenerate() {
        let mut r = Xorshift128::new(0);
        let vals: Vec<u64> = (0..10).map(|_| r.next_u64()).collect();
        assert!(vals.iter().any(|&v| v != 0));
        assert!(vals.windows(2).any(|w| w[0] != w[1]));
    }

    #[test]
    fn gen_range_stays_in_bounds_and_covers() {
        let mut r = Xorshift128::new(7);
        let mut seen = [false; 8];
        for _ in 0..10_000 {
            let v = r.gen_range(0..8);
            assert!(v < 8);
            seen[v as usize] = true;
        }
        assert!(seen.iter().all(|&s| s), "all 8 buckets hit over 10k draws");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jd-core rng -- --nocapture`
Expected: compile error — `Xorshift128` not defined.

- [ ] **Step 3: Implement**

```rust
//! Xorshift128+ — non-cryptographic PRNG (Vigna). Used for ULID entropy
//! and randomized test generation. Deliberately not `rand`: spec Appendix B.

use std::ops::Range;

pub struct Xorshift128(pub [u64; 2]);

/// SplitMix64 — seeds the xorshift state so nearby seeds produce unrelated streams.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl Xorshift128 {
    pub fn new(seed: u64) -> Self {
        let mut s = seed;
        let a = splitmix64(&mut s);
        let b = splitmix64(&mut s);
        // xorshift state must never be all-zero
        Xorshift128([a | 1, b])
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut s1 = self.0[0];
        let s0 = self.0[1];
        let result = s0.wrapping_add(s1);
        self.0[0] = s0;
        s1 ^= s1 << 23;
        self.0[1] = s1 ^ s0 ^ (s1 >> 18) ^ (s0 >> 5);
        result
    }

    /// Modulo bias is acceptable for our uses (tests, ULID entropy).
    pub fn gen_range(&mut self, range: Range<u64>) -> u64 {
        debug_assert!(range.start < range.end);
        range.start + self.next_u64() % (range.end - range.start)
    }
}
```

Add `pub mod rng;` to `lib.rs`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jd-core rng`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/jd-core
git commit -m "feat(core): xorshift128+ PRNG"
```

---

### Task 2: `time.rs` — Timestamp (RFC3339, UTC)

**Files:**
- Create: `crates/jd-core/src/time.rs`
- Modify: `crates/jd-core/src/lib.rs` (add `pub mod time;`)

**Interfaces:**
- Consumes: nothing.
- Produces: `Timestamp(pub i64)` (Unix millis, UTC), `Timestamp::now()`, `parse_rfc3339(&str) -> Result<Timestamp, TimeError>`, `to_rfc3339(&self) -> String` (canonical `YYYY-MM-DDTHH:MM:SSZ`, second precision), `days_since(&self, other) -> f64`, `pub enum TimeError`. Consumed by Task 3 (ULID timestamp), Task 6 (frontmatter created/modified), and nearly everything later.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_zero() {
        assert_eq!(Timestamp(0).to_rfc3339(), "1970-01-01T00:00:00Z");
        assert_eq!(Timestamp::parse_rfc3339("1970-01-01T00:00:00Z").unwrap(), Timestamp(0));
    }

    #[test]
    fn known_value_y2k() {
        // 2000-01-01T00:00:00Z = 946_684_800 seconds after epoch (well-known constant)
        assert_eq!(
            Timestamp::parse_rfc3339("2000-01-01T00:00:00Z").unwrap(),
            Timestamp(946_684_800_000)
        );
        assert_eq!(Timestamp(946_684_800_000).to_rfc3339(), "2000-01-01T00:00:00Z");
    }

    #[test]
    fn canonical_strings_round_trip() {
        for s in [
            "2026-07-03T10:22:00Z",
            "1999-12-31T23:59:59Z",
            "2024-02-29T12:00:00Z", // leap day
            "2100-01-01T00:00:00Z",
            "1969-12-31T23:59:59Z", // pre-epoch (negative millis)
        ] {
            let t = Timestamp::parse_rfc3339(s).unwrap();
            assert_eq!(t.to_rfc3339(), s, "round-trip of {s}");
        }
    }

    #[test]
    fn offsets_normalize_to_utc() {
        let utc = Timestamp::parse_rfc3339("2026-07-03T10:22:00Z").unwrap();
        assert_eq!(Timestamp::parse_rfc3339("2026-07-03T12:22:00+02:00").unwrap(), utc);
        assert_eq!(Timestamp::parse_rfc3339("2026-07-03T04:52:00-05:30").unwrap(), utc);
    }

    #[test]
    fn fractional_seconds_kept_as_millis() {
        assert_eq!(
            Timestamp::parse_rfc3339("1970-01-01T00:00:00.5Z").unwrap(),
            Timestamp(500)
        );
        // truncated beyond millis, not rounded
        assert_eq!(
            Timestamp::parse_rfc3339("1970-01-01T00:00:00.1239Z").unwrap(),
            Timestamp(123)
        );
    }

    #[test]
    fn lowercase_t_and_z_accepted() {
        assert_eq!(
            Timestamp::parse_rfc3339("1970-01-01t00:00:00z").unwrap(),
            Timestamp(0)
        );
    }

    #[test]
    fn rejects_invalid() {
        for s in [
            "2023-02-29T00:00:00Z", // not a leap year
            "2026-13-01T00:00:00Z", // month 13
            "2026-07-32T00:00:00Z", // day 32
            "2026-07-03T24:00:00Z", // hour 24
            "2026-07-03T10:22:00",  // missing offset
            "2026-07-03",           // date only
            "2026-07-03T10:22:00Zx", // trailing garbage
            "not a date",
            "",
        ] {
            assert!(Timestamp::parse_rfc3339(s).is_err(), "should reject {s:?}");
        }
    }

    #[test]
    fn days_since_works() {
        let a = Timestamp::parse_rfc3339("2026-07-01T00:00:00Z").unwrap();
        let b = Timestamp::parse_rfc3339("2026-07-04T12:00:00Z").unwrap();
        assert!((b.days_since(a) - 3.5).abs() < 1e-9);
        assert!((a.days_since(b) + 3.5).abs() < 1e-9);
    }

    #[test]
    fn now_is_plausible() {
        let now = Timestamp::now();
        let floor = Timestamp::parse_rfc3339("2026-01-01T00:00:00Z").unwrap();
        let ceil = Timestamp::parse_rfc3339("2100-01-01T00:00:00Z").unwrap();
        assert!(now > floor && now < ceil);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jd-core time`
Expected: compile error — `Timestamp` not defined.

- [ ] **Step 3: Implement**

Civil-date conversion uses Howard Hinnant's `days_from_civil` / `civil_from_days` algorithms (public domain, exact over the full i64 range we care about):

```rust
//! RFC3339 timestamps, written in-house (spec Appendix B rejects chrono/time).
//! We emit second-precision UTC only ("2026-07-03T10:22:00Z"); we accept
//! fractional seconds and numeric offsets on parse and normalize to UTC.

/// Milliseconds since Unix epoch, always UTC.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Timestamp(pub i64);

#[derive(Debug, PartialEq, Eq)]
pub enum TimeError {
    TooShort,
    BadSeparator,
    BadDigit,
    BadDate,
    BadTime,
    BadOffset,
    TrailingGarbage,
}

/// Days since 1970-01-01 for a civil date (Hinnant).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64; // [0, 11], March-based
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Civil date for days since 1970-01-01 (Hinnant).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn is_leap(y: i64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => if is_leap(y) { 29 } else { 28 },
        _ => 0,
    }
}

/// Parse exactly `n` ASCII digits at `b[at..at+n]`.
fn digits(b: &[u8], at: usize, n: usize) -> Result<i64, TimeError> {
    let slice = b.get(at..at + n).ok_or(TimeError::TooShort)?;
    let mut v: i64 = 0;
    for &c in slice {
        if !c.is_ascii_digit() {
            return Err(TimeError::BadDigit);
        }
        v = v * 10 + (c - b'0') as i64;
    }
    Ok(v)
}

impl Timestamp {
    pub fn now() -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let d = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before 1970");
        Timestamp(d.as_millis() as i64)
    }

    pub fn parse_rfc3339(s: &str) -> Result<Self, TimeError> {
        let b = s.as_bytes();
        if b.len() < 20 {
            return Err(TimeError::TooShort);
        }
        let sep_ok = b[4] == b'-'
            && b[7] == b'-'
            && (b[10] == b'T' || b[10] == b't' || b[10] == b' ')
            && b[13] == b':'
            && b[16] == b':';
        if !sep_ok {
            return Err(TimeError::BadSeparator);
        }
        let y = digits(b, 0, 4)?;
        let m = digits(b, 5, 2)? as u32;
        let d = digits(b, 8, 2)? as u32;
        let h = digits(b, 11, 2)?;
        let min = digits(b, 14, 2)?;
        let mut sec = digits(b, 17, 2)?;
        if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
            return Err(TimeError::BadDate);
        }
        if h > 23 || min > 59 || sec > 60 {
            return Err(TimeError::BadTime);
        }
        if sec == 60 {
            sec = 59; // leap second: clamp
        }

        let mut i = 19;
        let mut millis: i64 = 0;
        if b.get(i) == Some(&b'.') {
            i += 1;
            let start = i;
            while b.get(i).is_some_and(|c| c.is_ascii_digit()) {
                i += 1;
            }
            if i == start {
                return Err(TimeError::BadDigit);
            }
            // take the first 3 fraction digits as millis, right-padding
            let frac = &b[start..(start + 3).min(i)];
            let mut v: i64 = 0;
            for &c in frac {
                v = v * 10 + (c - b'0') as i64;
            }
            for _ in frac.len()..3 {
                v *= 10;
            }
            millis = v;
        }

        let offset_secs: i64 = match b.get(i) {
            Some(&b'Z') | Some(&b'z') => {
                i += 1;
                0
            }
            Some(&sign) if sign == b'+' || sign == b'-' => {
                let oh = digits(b, i + 1, 2)?;
                if b.get(i + 3) != Some(&b':') {
                    return Err(TimeError::BadOffset);
                }
                let om = digits(b, i + 4, 2)?;
                if oh > 23 || om > 59 {
                    return Err(TimeError::BadOffset);
                }
                i += 6;
                let secs = oh * 3600 + om * 60;
                if sign == b'+' { secs } else { -secs }
            }
            _ => return Err(TimeError::BadOffset),
        };
        if i != b.len() {
            return Err(TimeError::TrailingGarbage);
        }

        let day_secs = h * 3600 + min * 60 + sec;
        let epoch_secs = days_from_civil(y, m, d) * 86_400 + day_secs - offset_secs;
        Ok(Timestamp(epoch_secs * 1000 + millis))
    }

    /// Canonical form: second precision, UTC, uppercase Z. Millis are truncated.
    pub fn to_rfc3339(&self) -> String {
        let secs = self.0.div_euclid(1000);
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let (y, m, d) = civil_from_days(days);
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            y,
            m,
            d,
            rem / 3600,
            (rem % 3600) / 60,
            rem % 60
        )
    }

    pub fn days_since(&self, other: Timestamp) -> f64 {
        (self.0 - other.0) as f64 / 86_400_000.0
    }
}
```

Add `pub mod time;` to `lib.rs`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jd-core time`
Expected: 9 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/jd-core
git commit -m "feat(core): RFC3339 Timestamp with civil-date conversion"
```

---

### Task 3: `id.rs` — NoteId (ULID) + IdGen

**Files:**
- Create: `crates/jd-core/src/id.rs`
- Modify: `crates/jd-core/src/lib.rs` (add `pub mod id;`)

**Interfaces:**
- Consumes: `crate::rng::Xorshift128`, `crate::time::Timestamp`.
- Produces: `NoteId(pub [u8; 16])` with `Display` (26-char Crockford base32), `NoteId::generate(gen: &mut IdGen) -> NoteId`, `NoteId::parse(&str) -> Result<NoteId, IdError>`, `NoteId::short(&self) -> String` (first 8 chars), `NoteId::timestamp_ms(&self) -> u64`, `IdGen::new()`, `pub enum IdError`. Consumed everywhere a note identity exists.

- [ ] **Step 1: Write the failing tests**

```rust
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
        let mut gen = IdGen::new();
        for _ in 0..100 {
            let id = NoteId::generate(&mut gen);
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
        let mut gen = IdGen::new();
        let before = crate::time::Timestamp::now().0 as u64;
        let id = NoteId::generate(&mut gen);
        let after = crate::time::Timestamp::now().0 as u64;
        assert!(id.timestamp_ms() >= before && id.timestamp_ms() <= after);
    }

    #[test]
    fn generate_is_monotonic() {
        let mut gen = IdGen::new();
        let mut prev = NoteId::generate(&mut gen);
        for _ in 0..1000 {
            let next = NoteId::generate(&mut gen);
            assert!(next > prev, "ULIDs must be strictly increasing within a process");
            prev = next;
        }
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jd-core id`
Expected: compile error — `NoteId` not defined.

- [ ] **Step 3: Implement**

```rust
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
        b'A' => Some(10), b'B' => Some(11), b'C' => Some(12), b'D' => Some(13),
        b'E' => Some(14), b'F' => Some(15), b'G' => Some(16), b'H' => Some(17),
        b'J' => Some(18), b'K' => Some(19), b'M' => Some(20), b'N' => Some(21),
        b'P' => Some(22), b'Q' => Some(23), b'R' => Some(24), b'S' => Some(25),
        b'T' => Some(26), b'V' => Some(27), b'W' => Some(28), b'X' => Some(29),
        b'Y' => Some(30), b'Z' => Some(31),
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
    pub fn generate(gen: &mut IdGen) -> Self {
        let now_ms = Timestamp::now().0.max(0) as u64;
        let id = match gen.last {
            Some(last) if last.timestamp_ms() >= now_ms => {
                // +1 on the full 128 bits; carry past the 80-bit random part
                // into the timestamp is astronomically unlikely and harmless.
                NoteId(u128::from_be_bytes(last.0).wrapping_add(1).to_be_bytes())
            }
            _ => {
                let mut bytes = [0u8; 16];
                bytes[..6].copy_from_slice(&now_ms.to_be_bytes()[2..]);
                bytes[6..14].copy_from_slice(&gen.rng.next_u64().to_be_bytes());
                bytes[14..16].copy_from_slice(&gen.rng.next_u64().to_be_bytes()[..2]);
                NoteId(bytes)
            }
        };
        gen.last = Some(id);
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
```

Add `pub mod id;` to `lib.rs`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jd-core id`
Expected: 7 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/jd-core
git commit -m "feat(core): ULID NoteId with monotonic IdGen"
```

---

### Task 4: `tag.rs` — Tag

**Files:**
- Create: `crates/jd-core/src/tag.rs`
- Modify: `crates/jd-core/src/lib.rs` (add `pub mod tag;`)

**Interfaces:**
- Consumes: nothing.
- Produces: `Tag::new(raw: &str) -> Option<Tag>`, `as_str(&self) -> &str`, `matches(&self, other: &Tag) -> bool`, `fold_key(&self) -> String`. Consumed by Task 5/8, WP1c's tag index.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn tag(s: &str) -> Tag {
        Tag::new(s).unwrap()
    }

    #[test]
    fn normalizes_to_lowercase_and_strips_hash() {
        assert_eq!(tag("Rust").as_str(), "rust");
        assert_eq!(tag("#rust").as_str(), "rust");
        assert_eq!(tag("#Zettelkasten").as_str(), "zettelkasten");
        assert_eq!(tag("  method  ").as_str(), "method");
    }

    #[test]
    fn rejects_empty_and_whitespace() {
        assert!(Tag::new("").is_none());
        assert!(Tag::new("#").is_none());
        assert!(Tag::new("   ").is_none());
        assert!(Tag::new("two words").is_none());
    }

    #[test]
    fn plural_insensitive_matching() {
        // plain -s
        assert!(tag("book").matches(&tag("books")));
        assert!(tag("books").matches(&tag("book")));
        // -es after s/x/z/ch/sh stems
        assert!(tag("box").matches(&tag("boxes")));
        assert!(tag("class").matches(&tag("classes")));
        assert!(tag("branch").matches(&tag("branches")));
        // "notes" folds by the plain-s rule (stem "not" doesn't take -es)
        assert!(tag("note").matches(&tag("notes")));
    }

    #[test]
    fn ss_endings_do_not_fold() {
        assert_eq!(tag("boss").fold_key(), "boss");
        assert!(!tag("boss").matches(&tag("bos")));
    }

    #[test]
    fn identical_tags_match() {
        assert!(tag("rust").matches(&tag("rust")));
        assert!(!tag("rust").matches(&tag("python")));
    }

    #[test]
    fn fold_keys() {
        assert_eq!(tag("books").fold_key(), "book");
        assert_eq!(tag("boxes").fold_key(), "box");
        assert_eq!(tag("classes").fold_key(), "class");
        assert_eq!(tag("rust").fold_key(), "rust");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jd-core tag`
Expected: compile error — `Tag` not defined.

- [ ] **Step 3: Implement**

```rust
//! Tags: flat, lowercase, plural-insensitive matching (spec §2).
//! The fold is a deliberate heuristic — "bus" folds to "bu", which is fine:
//! both sides of every comparison fold the same way.

/// Stored lowercase, `#` and surrounding whitespace stripped. No nesting.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Tag(String);

impl Tag {
    pub fn new(raw: &str) -> Option<Tag> {
        let s = raw.trim().trim_start_matches('#').trim();
        if s.is_empty() || s.chars().any(char::is_whitespace) {
            return None;
        }
        Some(Tag(s.to_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Canonical singular-ish form used as the index bucket key.
    pub fn fold_key(&self) -> String {
        let s = &self.0;
        if s.len() > 2 && s.ends_with("es") {
            let stem = &s[..s.len() - 2];
            if stem.ends_with('s')
                || stem.ends_with('x')
                || stem.ends_with('z')
                || stem.ends_with("ch")
                || stem.ends_with("sh")
            {
                return stem.to_owned();
            }
        }
        if s.len() > 1 && s.ends_with('s') && !s.ends_with("ss") {
            return s[..s.len() - 1].to_owned();
        }
        s.clone()
    }

    pub fn matches(&self, other: &Tag) -> bool {
        self.fold_key() == other.fold_key()
    }
}
```

Add `pub mod tag;` to `lib.rs`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jd-core tag`
Expected: 6 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/jd-core
git commit -m "feat(core): Tag with plural-insensitive fold"
```

---

### Task 5: `note.rs` — Status, Kind, NoteMeta, LinkRef, NewNote

**Files:**
- Create: `crates/jd-core/src/note.rs`
- Modify: `crates/jd-core/src/lib.rs` (add `pub mod note;`)

**Interfaces:**
- Consumes: `NoteId`, `Timestamp`, `Tag`.
- Produces: exactly the types in architecture doc §2.5, plus `Status::as_str`/`Status::parse` and `Kind::as_str`/`Kind::parse` (used by the frontmatter parser in Task 6).

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_round_trips() {
        assert_eq!(Status::parse("fleeting"), Some(Status::Fleeting));
        assert_eq!(Status::parse("permanent"), Some(Status::Permanent));
        assert_eq!(Status::parse("Fleeting"), Some(Status::Fleeting)); // case-insensitive
        assert_eq!(Status::parse("draft"), None);
        assert_eq!(Status::Fleeting.as_str(), "fleeting");
        assert_eq!(Status::Permanent.as_str(), "permanent");
    }

    #[test]
    fn kind_round_trips_and_defaults() {
        assert_eq!(Kind::parse("note"), Some(Kind::Note));
        assert_eq!(Kind::parse("literature"), Some(Kind::Literature));
        assert_eq!(Kind::parse("structure"), Some(Kind::Structure));
        assert_eq!(Kind::parse("recipe"), None);
        assert_eq!(Kind::default(), Kind::Note);
        assert_eq!(Kind::Literature.as_str(), "literature");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jd-core note`
Expected: compile error.

- [ ] **Step 3: Implement**

```rust
//! Note domain types. Lifecycle (`Status`) and what-it-is (`Kind`) are
//! orthogonal axes (spec §2). Bodies never live in these types — the index
//! holds metadata only.

use std::collections::BTreeSet;
use std::ops::Range;
use std::path::PathBuf;

use crate::id::NoteId;
use crate::tag::Tag;
use crate::time::Timestamp;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Fleeting,
    Permanent,
}

impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Fleeting => "fleeting",
            Status::Permanent => "permanent",
        }
    }

    pub fn parse(s: &str) -> Option<Status> {
        match s.to_ascii_lowercase().as_str() {
            "fleeting" => Some(Status::Fleeting),
            "permanent" => Some(Status::Permanent),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Kind {
    #[default]
    Note,
    Literature,
    Structure,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Note => "note",
            Kind::Literature => "literature",
            Kind::Structure => "structure",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        match s.to_ascii_lowercase().as_str() {
            "note" => Some(Kind::Note),
            "literature" => Some(Kind::Literature),
            "structure" => Some(Kind::Structure),
            _ => None,
        }
    }
}

/// A `[[wikilink]]` occurrence in a body.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkRef {
    /// Raw title text inside the brackets, pipe part excluded.
    pub target: String,
    /// Text after `|`, if any.
    pub display: Option<String>,
    /// Byte range in the body, including the brackets.
    pub span: Range<usize>,
}

/// Everything the index holds about a note (spec §3: bodies are NOT here).
#[derive(Clone, Debug)]
pub struct NoteMeta {
    pub id: NoteId,
    /// Relative to the vault root, e.g. "notes/Egui tradeoffs.md".
    pub rel_path: PathBuf,
    /// First `# ` heading in the body; None for untitled scraps.
    pub title: Option<String>,
    /// First non-empty body line — scrap display and a11y announcements.
    pub first_line: String,
    pub status: Status,
    pub kind: Kind,
    pub source: Option<String>,
    pub created: Timestamp,
    pub modified: Timestamp,
    /// Union of the frontmatter list and #inline-tags.
    pub tags: BTreeSet<Tag>,
    pub links_out: Vec<LinkRef>,
    pub word_count: u32,
}

/// Seed for creating a note (capture paths, palette "New scrap", split).
#[derive(Clone, Debug)]
pub struct NewNote {
    pub body: String,
    pub status: Status,
    pub kind: Kind,
    pub source: Option<String>,
    pub tags: Vec<Tag>,
}
```

Add `pub mod note;` to `lib.rs`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jd-core note`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/jd-core
git commit -m "feat(core): note domain types"
```

---

### Task 6: `frontmatter.rs` — parse + serialize (the byte-identity core)

**Files:**
- Create: `crates/jd-core/src/frontmatter.rs`
- Modify: `crates/jd-core/src/lib.rs` (add `pub mod frontmatter;`)

**Interfaces:**
- Consumes: `NoteId`, `Timestamp`, `Status`, `Kind`, `Tag`.
- Produces: `FrontmatterDoc::parse(input: &str) -> Result<(FrontmatterDoc, usize), FmError>` (usize = bytes consumed through the closing `---` line), `FrontmatterDoc::empty()`, `FrontmatterDoc::synthesize(id, created, status)`, accessors `id() -> Option<NoteId>`, `created()/modified() -> Option<Timestamp>`, `status() -> Option<Status>`, `kind() -> Kind`, `source() -> Option<&str>` *(returned as `Option<String>` — value extraction unquotes, so a borrowed return isn't possible; architecture doc updated)*, `tags() -> Vec<Tag>`, `serialize() -> String`, `pub enum FmError { NoOpeningMarker, Unterminated }`, `pub enum KnownKey`. Setters land in Task 7.

**Design (from architecture doc §2.6):** the doc stores every original line raw (including its terminator); known keys are *tagged*, and typed accessors re-parse the tagged line's value on demand (notes are ~1 KB; this is cheap and keeps one source of truth). `serialize()` concatenates raw lines — byte-identical unless a setter rewrote a line. Value syntax: `key: value` scalars, optional single/double quotes, inline lists `[a, b]`; block lists (`- item` lines under `tags:`) parse but canonical write is inline. Everything else preserves raw and uninterpreted.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::note::{Kind, Status};

    const SPEC_EXAMPLE: &str = "---\n\
id: 01J8ZQ4KF3T9M2X7C5VBNAE8RD\n\
created: 2026-07-03T10:22:00Z\n\
modified: 2026-07-04T09:10:00Z\n\
status: permanent\n\
kind: literature\n\
source: \"Ahrens, How to Take Smart Notes (2017)\"\n\
tags: [zettelkasten, method]\n\
---\n";

    #[test]
    fn parses_the_spec_example() {
        let (fm, consumed) = FrontmatterDoc::parse(SPEC_EXAMPLE).unwrap();
        assert_eq!(consumed, SPEC_EXAMPLE.len());
        assert_eq!(fm.id().unwrap().to_string(), "01J8ZQ4KF3T9M2X7C5VBNAE8RD");
        assert_eq!(fm.status(), Some(Status::Permanent));
        assert_eq!(fm.kind(), Kind::Literature);
        assert_eq!(fm.source().as_deref(), Some("Ahrens, How to Take Smart Notes (2017)"));
        assert_eq!(
            fm.tags().iter().map(|t| t.as_str().to_owned()).collect::<Vec<_>>(),
            vec!["zettelkasten", "method"]
        );
        assert_eq!(
            fm.created().unwrap(),
            crate::time::Timestamp::parse_rfc3339("2026-07-03T10:22:00Z").unwrap()
        );
    }

    #[test]
    fn serialize_is_byte_identical() {
        for input in [
            SPEC_EXAMPLE,
            "---\nid: 01J8ZQ4KF3T9M2X7C5VBNAE8RD\n---\n",
            // unknown keys, weird spacing, comments — all preserved verbatim
            "---\nid: 01J8ZQ4KF3T9M2X7C5VBNAE8RD\nobsidian-ui-mode: preview\naliases: [a, b]\n  weird indent line\n---\n",
            // CRLF terminators
            "---\r\nid: 01J8ZQ4KF3T9M2X7C5VBNAE8RD\r\nstatus: fleeting\r\n---\r\n",
            // single-quoted values, extra whitespace around colon
            "---\nsource: 'single quoted'\nstatus:   fleeting\n---\n",
        ] {
            let (fm, consumed) = FrontmatterDoc::parse(input).unwrap();
            assert_eq!(consumed, input.len());
            assert_eq!(fm.serialize(), input, "round-trip of {input:?}");
        }
    }

    #[test]
    fn consumed_stops_at_closing_marker() {
        let input = "---\nstatus: fleeting\n---\nBody text here.\n";
        let (fm, consumed) = FrontmatterDoc::parse(input).unwrap();
        assert_eq!(&input[consumed..], "Body text here.\n");
        assert_eq!(fm.serialize(), &input[..consumed]);
    }

    #[test]
    fn block_list_tags_parse() {
        let input = "---\ntags:\n  - zettelkasten\n  - method\n---\n";
        let (fm, _) = FrontmatterDoc::parse(input).unwrap();
        assert_eq!(
            fm.tags().iter().map(|t| t.as_str().to_owned()).collect::<Vec<_>>(),
            vec!["zettelkasten", "method"]
        );
        assert_eq!(fm.serialize(), input);
    }

    #[test]
    fn missing_and_absent_fields() {
        let (fm, _) = FrontmatterDoc::parse("---\nstatus: fleeting\n---\n").unwrap();
        assert_eq!(fm.id(), None);
        assert_eq!(fm.kind(), Kind::Note); // absent means note
        assert_eq!(fm.source(), None);
        assert!(fm.tags().is_empty());
        assert_eq!(fm.created(), None);
    }

    #[test]
    fn garbage_values_read_as_none_but_preserve() {
        let input = "---\nid: not-a-ulid\nstatus: draft\nkind: recipe\n---\n";
        let (fm, _) = FrontmatterDoc::parse(input).unwrap();
        assert_eq!(fm.id(), None);
        assert_eq!(fm.status(), None);
        assert_eq!(fm.kind(), Kind::Note);
        assert_eq!(fm.serialize(), input);
    }

    #[test]
    fn errors() {
        assert!(matches!(
            FrontmatterDoc::parse("# Just a body\n"),
            Err(FmError::NoOpeningMarker)
        ));
        assert!(matches!(
            FrontmatterDoc::parse("---\nstatus: fleeting\n"),
            Err(FmError::Unterminated)
        ));
    }

    #[test]
    fn empty_doc_serializes_to_nothing() {
        assert_eq!(FrontmatterDoc::empty().serialize(), "");
    }

    #[test]
    fn synthesize_produces_canonical_block() {
        let id = crate::id::NoteId::parse("01J8ZQ4KF3T9M2X7C5VBNAE8RD").unwrap();
        let t = crate::time::Timestamp::parse_rfc3339("2026-07-03T10:22:00Z").unwrap();
        let fm = FrontmatterDoc::synthesize(id, t, Status::Fleeting);
        assert_eq!(
            fm.serialize(),
            "---\n\
id: 01J8ZQ4KF3T9M2X7C5VBNAE8RD\n\
created: 2026-07-03T10:22:00Z\n\
modified: 2026-07-03T10:22:00Z\n\
status: fleeting\n\
---\n"
        );
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jd-core frontmatter`
Expected: compile error.

- [ ] **Step 3: Implement**

```rust
//! Fixed-schema frontmatter with byte-identity round-trips (spec §2).
//! Mechanism: every original line is kept raw (terminator included); known
//! keys are tagged; accessors re-parse tagged lines on demand; setters
//! (Task 7) rewrite only their own line. `serialize` concatenates raw lines.

use crate::id::NoteId;
use crate::note::{Kind, Status};
use crate::tag::Tag;
use crate::time::Timestamp;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KnownKey {
    Id,
    Created,
    Modified,
    Status,
    Kind,
    Source,
    Tags,
}

impl KnownKey {
    fn from_name(name: &str) -> Option<KnownKey> {
        match name {
            "id" => Some(KnownKey::Id),
            "created" => Some(KnownKey::Created),
            "modified" => Some(KnownKey::Modified),
            "status" => Some(KnownKey::Status),
            "kind" => Some(KnownKey::Kind),
            "source" => Some(KnownKey::Source),
            "tags" => Some(KnownKey::Tags),
            _ => None,
        }
    }

    pub(crate) fn name(&self) -> &'static str {
        match self {
            KnownKey::Id => "id",
            KnownKey::Created => "created",
            KnownKey::Modified => "modified",
            KnownKey::Status => "status",
            KnownKey::Kind => "kind",
            KnownKey::Source => "source",
            KnownKey::Tags => "tags",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum LineRole {
    Marker,                       // the --- lines
    Key(KnownKey),                // a recognized `key: value` line
    Continuation(KnownKey),       // `- item` under a known key with empty value
    Other,                        // unknown key, comment, anything — preserved raw
}

#[derive(Clone, Debug)]
pub(crate) struct FmLine {
    /// Full original line INCLUDING its terminator (\n or \r\n; last line may have none).
    pub(crate) raw: String,
    pub(crate) role: LineRole,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FmError {
    NoOpeningMarker,
    Unterminated,
}

#[derive(Clone, Debug)]
pub struct FrontmatterDoc {
    pub(crate) lines: Vec<FmLine>, // covers opening marker..closing marker inclusive; empty = no block
}

/// Split into lines, each keeping its terminator.
fn lines_inclusive(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, b) in s.bytes().enumerate() {
        if b == b'\n' {
            out.push(&s[start..=i]);
            start = i + 1;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// The line's content without its terminator.
fn content(raw: &str) -> &str {
    raw.trim_end_matches('\n').trim_end_matches('\r')
}

/// Strip one matching pair of single or double quotes.
fn unquote(s: &str) -> &str {
    let s = s.trim();
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"'))
            || (s.starts_with('\'') && s.ends_with('\'')))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// `key: value` → (key, value) if key is a plain identifier at column 0.
fn split_key_line(line: &str) -> Option<(&str, &str)> {
    let colon = line.find(':')?;
    let key = &line[..colon];
    if key.is_empty()
        || !key.chars().next().unwrap().is_ascii_alphabetic()
        || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    Some((key, line[colon + 1..].trim()))
}

impl FrontmatterDoc {
    pub fn empty() -> FrontmatterDoc {
        FrontmatterDoc { lines: Vec::new() }
    }

    pub fn parse(input: &str) -> Result<(FrontmatterDoc, usize), FmError> {
        let all = lines_inclusive(input);
        let first = all.first().ok_or(FmError::NoOpeningMarker)?;
        if content(first) != "---" {
            return Err(FmError::NoOpeningMarker);
        }

        let mut lines = vec![FmLine { raw: first.to_string(), role: LineRole::Marker }];
        let mut consumed = first.len();
        let mut open_list_key: Option<KnownKey> = None;
        let mut closed = false;

        for raw in &all[1..] {
            let c = content(raw);
            consumed += raw.len();
            if c == "---" {
                lines.push(FmLine { raw: raw.to_string(), role: LineRole::Marker });
                closed = true;
                break;
            }
            let role = if let Some(key) = open_list_key.filter(|_| c.trim_start().starts_with("- ")) {
                LineRole::Continuation(key)
            } else if let Some((name, value)) = split_key_line(c) {
                match KnownKey::from_name(name) {
                    Some(k) => {
                        open_list_key = (k == KnownKey::Tags && value.is_empty()).then_some(k);
                        LineRole::Key(k)
                    }
                    None => {
                        open_list_key = None;
                        LineRole::Other
                    }
                }
            } else {
                if !c.trim_start().starts_with("- ") {
                    open_list_key = None;
                }
                LineRole::Other
            };
            lines.push(FmLine { raw: raw.to_string(), role });
        }

        if !closed {
            return Err(FmError::Unterminated);
        }
        Ok((FrontmatterDoc { lines }, consumed))
    }

    pub fn synthesize(id: NoteId, created: Timestamp, status: Status) -> FrontmatterDoc {
        let text = format!(
            "---\nid: {id}\ncreated: {c}\nmodified: {c}\nstatus: {s}\n---\n",
            c = created.to_rfc3339(),
            s = status.as_str(),
        );
        FrontmatterDoc::parse(&text).expect("synthesized block always parses").0
    }

    pub fn serialize(&self) -> String {
        self.lines.iter().map(|l| l.raw.as_str()).collect()
    }

    /// The unquoted scalar value of a known key's line, if present.
    fn value_of(&self, key: KnownKey) -> Option<String> {
        self.lines.iter().find_map(|l| match l.role {
            LineRole::Key(k) if k == key => {
                let (_, v) = split_key_line(content(&l.raw))?;
                Some(unquote(v).to_owned())
            }
            _ => None,
        })
    }

    pub fn id(&self) -> Option<NoteId> {
        NoteId::parse(&self.value_of(KnownKey::Id)?).ok()
    }

    pub fn created(&self) -> Option<Timestamp> {
        Timestamp::parse_rfc3339(&self.value_of(KnownKey::Created)?).ok()
    }

    pub fn modified(&self) -> Option<Timestamp> {
        Timestamp::parse_rfc3339(&self.value_of(KnownKey::Modified)?).ok()
    }

    pub fn status(&self) -> Option<Status> {
        Status::parse(&self.value_of(KnownKey::Status)?)
    }

    /// Absent or unrecognized means `Kind::Note` (spec §2).
    pub fn kind(&self) -> Kind {
        self.value_of(KnownKey::Kind)
            .and_then(|v| Kind::parse(&v))
            .unwrap_or_default()
    }

    pub fn source(&self) -> Option<String> {
        self.value_of(KnownKey::Source).filter(|s| !s.is_empty())
    }

    pub fn tags(&self) -> Vec<Tag> {
        // inline form: tags: [a, b] — or a bare scalar for a single tag
        if let Some(v) = self.value_of(KnownKey::Tags) {
            if !v.is_empty() {
                let inner = v.strip_prefix('[').and_then(|s| s.strip_suffix(']')).unwrap_or(&v);
                return inner
                    .split(',')
                    .filter_map(|item| Tag::new(unquote(item)))
                    .collect();
            }
        }
        // block form: continuation lines "- item"
        self.lines
            .iter()
            .filter_map(|l| match l.role {
                LineRole::Continuation(KnownKey::Tags) => {
                    let c = content(&l.raw).trim_start();
                    Tag::new(unquote(c.strip_prefix("- ")?))
                }
                _ => None,
            })
            .collect()
    }
}
```

Add `pub mod frontmatter;` to `lib.rs`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jd-core frontmatter`
Expected: 9 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/jd-core
git commit -m "feat(core): byte-identity frontmatter parser"
```

---

### Task 7: Frontmatter setters

**Files:**
- Modify: `crates/jd-core/src/frontmatter.rs`

**Interfaces:**
- Consumes: Task 6's `FrontmatterDoc`.
- Produces: `set_status(&mut self, Status)`, `set_kind(&mut self, Kind)` (Note removes the line), `set_source(&mut self, Option<&str>)` (None removes), `set_modified(&mut self, Timestamp)`, `set_tags(&mut self, &[Tag])` (canonical inline `tags: [a, b]`; replaces block-list continuations). Consumed by WP1e's `VaultOp` execution.

- [ ] **Step 1: Write the failing tests** (append to the test module)

```rust
    const SETTER_BASE: &str = "---\n\
id: 01J8ZQ4KF3T9M2X7C5VBNAE8RD\n\
x-custom: keep me\n\
status: fleeting\n\
---\n";

    #[test]
    fn set_status_rewrites_only_its_line() {
        let (mut fm, _) = FrontmatterDoc::parse(SETTER_BASE).unwrap();
        fm.set_status(Status::Permanent);
        assert_eq!(
            fm.serialize(),
            "---\n\
id: 01J8ZQ4KF3T9M2X7C5VBNAE8RD\n\
x-custom: keep me\n\
status: permanent\n\
---\n"
        );
    }

    #[test]
    fn set_preserves_crlf_terminator_of_the_line() {
        let input = "---\r\nstatus: fleeting\r\n---\r\n";
        let (mut fm, _) = FrontmatterDoc::parse(input).unwrap();
        fm.set_status(Status::Permanent);
        assert_eq!(fm.serialize(), "---\r\nstatus: permanent\r\n---\r\n");
    }

    #[test]
    fn set_missing_key_appends_before_closing_marker() {
        let (mut fm, _) = FrontmatterDoc::parse(SETTER_BASE).unwrap();
        fm.set_source(Some("Ahrens (2017)"));
        assert_eq!(
            fm.serialize(),
            "---\n\
id: 01J8ZQ4KF3T9M2X7C5VBNAE8RD\n\
x-custom: keep me\n\
status: fleeting\n\
source: \"Ahrens (2017)\"\n\
---\n"
        );
    }

    #[test]
    fn set_kind_note_removes_the_line() {
        let input = "---\nkind: literature\nstatus: fleeting\n---\n";
        let (mut fm, _) = FrontmatterDoc::parse(input).unwrap();
        fm.set_kind(Kind::Note);
        assert_eq!(fm.serialize(), "---\nstatus: fleeting\n---\n");
        // and setting a non-default kind on a doc without the line adds it
        fm.set_kind(Kind::Structure);
        assert_eq!(fm.serialize(), "---\nstatus: fleeting\nkind: structure\n---\n");
    }

    #[test]
    fn set_source_none_removes() {
        let input = "---\nsource: \"x\"\nstatus: fleeting\n---\n";
        let (mut fm, _) = FrontmatterDoc::parse(input).unwrap();
        fm.set_source(None);
        assert_eq!(fm.serialize(), "---\nstatus: fleeting\n---\n");
    }

    #[test]
    fn set_tags_replaces_block_list_with_inline() {
        let input = "---\ntags:\n  - old-one\n  - old-two\nstatus: fleeting\n---\n";
        let (mut fm, _) = FrontmatterDoc::parse(input).unwrap();
        fm.set_tags(&[Tag::new("rust").unwrap(), Tag::new("egui").unwrap()]);
        assert_eq!(fm.serialize(), "---\ntags: [rust, egui]\nstatus: fleeting\n---\n");
    }

    #[test]
    fn set_modified_updates_timestamp() {
        let (mut fm, _) = FrontmatterDoc::parse(SETTER_BASE).unwrap();
        let t = crate::time::Timestamp::parse_rfc3339("2026-07-06T08:00:00Z").unwrap();
        fm.set_modified(t);
        assert_eq!(fm.modified(), Some(t));
        assert!(fm.serialize().contains("modified: 2026-07-06T08:00:00Z\n"));
        // untouched lines still verbatim
        assert!(fm.serialize().contains("x-custom: keep me\n"));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jd-core frontmatter`
Expected: compile errors — setters not defined.

- [ ] **Step 3: Implement** (append to `impl FrontmatterDoc`)

```rust
    /// The dominant terminator for appended lines (borrow the closing marker's).
    fn block_terminator(&self) -> &'static str {
        match self.lines.last().map(|l| l.raw.ends_with("\r\n")) {
            Some(true) => "\r\n",
            _ => "\n",
        }
    }

    /// Terminator of an existing line, defaulting to the block's.
    fn terminator_of(raw: &str, fallback: &'static str) -> &'static str {
        if raw.ends_with("\r\n") {
            "\r\n"
        } else if raw.ends_with('\n') {
            "\n"
        } else {
            fallback
        }
    }

    /// Rewrite key's line with `key: value`, or insert before the closing marker.
    /// `value = None` removes the line. Continuation lines of that key are always removed.
    fn set_raw(&mut self, key: KnownKey, value: Option<String>) {
        assert!(!self.lines.is_empty(), "cannot set fields on an empty frontmatter block");
        let fallback = self.block_terminator();
        self.lines
            .retain(|l| !matches!(l.role, LineRole::Continuation(k) if k == key));
        let existing = self
            .lines
            .iter()
            .position(|l| matches!(l.role, LineRole::Key(k) if k == key));
        match (existing, value) {
            (Some(i), Some(v)) => {
                let term = Self::terminator_of(&self.lines[i].raw, fallback);
                self.lines[i].raw = format!("{}: {}{}", key.name(), v, term);
            }
            (Some(i), None) => {
                self.lines.remove(i);
            }
            (None, Some(v)) => {
                let closing = self.lines.len() - 1; // the closing marker
                self.lines.insert(
                    closing,
                    FmLine {
                        raw: format!("{}: {}{}", key.name(), v, fallback),
                        role: LineRole::Key(key),
                    },
                );
            }
            (None, None) => {}
        }
    }

    pub fn set_status(&mut self, s: Status) {
        self.set_raw(KnownKey::Status, Some(s.as_str().to_owned()));
    }

    pub fn set_kind(&mut self, k: Kind) {
        let v = (k != Kind::Note).then(|| k.as_str().to_owned());
        self.set_raw(KnownKey::Kind, v);
    }

    pub fn set_source(&mut self, src: Option<&str>) {
        self.set_raw(KnownKey::Source, src.map(|s| format!("\"{}\"", s.replace('"', "'"))));
    }

    pub fn set_modified(&mut self, t: Timestamp) {
        self.set_raw(KnownKey::Modified, Some(t.to_rfc3339()));
    }

    pub fn set_tags(&mut self, tags: &[Tag]) {
        if tags.is_empty() {
            self.set_raw(KnownKey::Tags, None);
        } else {
            let list = tags.iter().map(Tag::as_str).collect::<Vec<_>>().join(", ");
            self.set_raw(KnownKey::Tags, Some(format!("[{list}]")));
        }
    }
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jd-core frontmatter`
Expected: all frontmatter tests pass (16 total).

- [ ] **Step 5: Commit**

```bash
git add crates/jd-core
git commit -m "feat(core): frontmatter setters that rewrite only their line"
```

---

### Task 8: `doc.rs` — NoteDoc + body extractors

**Files:**
- Create: `crates/jd-core/src/doc.rs`
- Modify: `crates/jd-core/src/lib.rs` (add `pub mod doc;`)

**Interfaces:**
- Consumes: `FrontmatterDoc`, `NoteMeta`, `LinkRef`, `Tag`, `Timestamp`, `Status`, `Kind`.
- Produces: `NoteDoc { pub fm: FrontmatterDoc, pub body: String }`, `NoteDoc::parse(&str) -> NoteDoc` (infallible — no frontmatter means empty fm + whole input as body), `serialize() -> String`, `to_meta(&self, id: NoteId, rel_path: &Path, fs_modified: Timestamp) -> NoteMeta` *(note: `id` is a parameter — the caller resolves identity when frontmatter lacks one; architecture doc updated)*, and pure helpers `extract_title`, `extract_links`, `extract_inline_tags`, `word_count`. Consumed by WP1b (lexer shares conventions), WP1c (index build), WP1d (scan).

**Pinned semantics:**
- Title = first `# ` (exactly one hash) heading outside code fences.
- Wikilinks: `[[target]]` / `[[target|display]]`, same-line only, skipped inside fenced code and inline code; empty targets ignored.
- Inline tags: `#word` where `#` is preceded by start-of-line or whitespace and followed by `[A-Za-z0-9_-]` starting alphanumeric; skipped in code. `# Title` is a heading, not a tag (space after `#`).
- `word_count` counts maximal alphanumeric runs.
- `first_line` = first non-empty body line, trimmed, leading `#`-marks + space stripped.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::note::{Kind, Status};
    use crate::time::Timestamp;
    use std::path::Path;

    #[test]
    fn parse_without_frontmatter_is_all_body() {
        let doc = NoteDoc::parse("# Just a note\n\nText.\n");
        assert_eq!(doc.body, "# Just a note\n\nText.\n");
        assert_eq!(doc.serialize(), "# Just a note\n\nText.\n");
    }

    #[test]
    fn parse_splits_frontmatter_and_body() {
        let input = "---\nstatus: fleeting\n---\n# Title\nBody.\n";
        let doc = NoteDoc::parse(input);
        assert_eq!(doc.body, "# Title\nBody.\n");
        assert_eq!(doc.serialize(), input);
    }

    #[test]
    fn broken_frontmatter_is_body_not_error() {
        // unterminated block: treat the whole file as body; round-trip untouched
        let input = "---\nstatus: fleeting\nno closing marker\n";
        let doc = NoteDoc::parse(input);
        assert_eq!(doc.serialize(), input);
    }

    #[test]
    fn extract_title_cases() {
        assert_eq!(
            extract_title("# The claim is the title\nbody"),
            Some(("The claim is the title".to_owned(), 2..24))
        );
        assert_eq!(extract_title("no heading here"), None);
        assert_eq!(extract_title("## h2 is not the title\n# but this is\n").unwrap().0, "but this is");
        // heading inside a code fence doesn't count
        assert_eq!(extract_title("```\n# not a title\n```\n# real title\n").unwrap().0, "real title");
        // first `# ` wins even if later ones exist
        assert_eq!(extract_title("intro line\n# First\n# Second\n").unwrap().0, "First");
    }

    #[test]
    fn extract_links_cases() {
        let body = "See [[Alpha]] and [[Beta|the beta note]].\n";
        let links = extract_links(body);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].target, "Alpha");
        assert_eq!(links[0].display, None);
        assert_eq!(&body[links[0].span.clone()], "[[Alpha]]");
        assert_eq!(links[1].target, "Beta");
        assert_eq!(links[1].display.as_deref(), Some("the beta note"));
        assert_eq!(&body[links[1].span.clone()], "[[Beta|the beta note]]");
    }

    #[test]
    fn links_skip_code_and_malformed() {
        assert!(extract_links("`[[not a link]]`").is_empty());
        assert!(extract_links("```\n[[not a link]]\n```\n").is_empty());
        assert!(extract_links("[[unclosed\n").is_empty());
        assert!(extract_links("[[]]").is_empty()); // empty target
        // spans across lines don't count
        assert!(extract_links("[[first\nsecond]]").is_empty());
    }

    #[test]
    fn extract_inline_tags_cases() {
        let tags: Vec<String> = extract_inline_tags("Uses #rust and #egui-widgets.\n#linestart too\n")
            .iter().map(|t| t.as_str().to_owned()).collect();
        assert_eq!(tags, vec!["rust", "egui-widgets", "linestart"]);
        // heading is not a tag; code is skipped; mid-word # is not a tag
        assert!(extract_inline_tags("# Heading\n").is_empty());
        assert!(extract_inline_tags("`#code`\n").is_empty());
        assert!(extract_inline_tags("C# is a language\n").is_empty());
    }

    #[test]
    fn word_count_cases() {
        assert_eq!(word_count(""), 0);
        assert_eq!(word_count("hello, world!"), 2);
        assert_eq!(word_count("héllo wörld"), 2);
        assert_eq!(word_count("# heading and [[link text]]"), 4);
    }

    #[test]
    fn to_meta_defaults_by_path_and_falls_back_to_fs_time() {
        let fs_t = Timestamp::parse_rfc3339("2026-07-05T00:00:00Z").unwrap();
        let id = crate::id::NoteId::parse("01J8ZQ4KF3T9M2X7C5VBNAE8RD").unwrap();

        // no frontmatter, in inbox/ → fleeting, fs timestamps
        let doc = NoteDoc::parse("a stray thought\n");
        let meta = doc.to_meta(id, Path::new("inbox/stray.md"), fs_t);
        assert_eq!(meta.status, Status::Fleeting);
        assert_eq!(meta.kind, Kind::Note);
        assert_eq!(meta.created, fs_t);
        assert_eq!(meta.modified, fs_t);
        assert_eq!(meta.title, None);
        assert_eq!(meta.first_line, "a stray thought");

        // notes/ default is permanent; frontmatter overrides win
        let doc = NoteDoc::parse("---\nstatus: fleeting\ntags: [zettel]\n---\n# T\nBody #inline\n");
        let meta = doc.to_meta(id, Path::new("notes/T.md"), fs_t);
        assert_eq!(meta.status, Status::Fleeting); // frontmatter wins over path default
        assert_eq!(meta.title.as_deref(), Some("T"));
        assert_eq!(meta.first_line, "T");
        let tags: Vec<String> = meta.tags.iter().map(|t| t.as_str().to_owned()).collect();
        assert_eq!(tags, vec!["inline", "zettel"]); // BTreeSet order; union of both sources

        let doc = NoteDoc::parse("body\n");
        let meta = doc.to_meta(id, Path::new("notes/x.md"), fs_t);
        assert_eq!(meta.status, Status::Permanent);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p jd-core doc`
Expected: compile error.

- [ ] **Step 3: Implement**

```rust
//! Whole-file view of a note: frontmatter + body, with the pure extractors
//! that turn a body into indexable metadata. Parsing is infallible — a file
//! that isn't note-shaped is still a valid body that round-trips untouched.

use std::collections::BTreeSet;
use std::ops::Range;
use std::path::Path;

use crate::frontmatter::{FmError, FrontmatterDoc};
use crate::id::NoteId;
use crate::note::{LinkRef, NoteMeta, Status};
use crate::tag::Tag;
use crate::time::Timestamp;

pub struct NoteDoc {
    pub fm: FrontmatterDoc,
    pub body: String,
}

impl NoteDoc {
    pub fn parse(input: &str) -> NoteDoc {
        match FrontmatterDoc::parse(input) {
            Ok((fm, consumed)) => NoteDoc { fm, body: input[consumed..].to_owned() },
            Err(FmError::NoOpeningMarker) | Err(FmError::Unterminated) => {
                NoteDoc { fm: FrontmatterDoc::empty(), body: input.to_owned() }
            }
        }
    }

    pub fn serialize(&self) -> String {
        let mut out = self.fm.serialize();
        out.push_str(&self.body);
        out
    }

    /// `id` comes from the caller: frontmatter if present, else assigned at scan.
    pub fn to_meta(&self, id: NoteId, rel_path: &Path, fs_modified: Timestamp) -> NoteMeta {
        let path_default = if rel_path.starts_with("inbox") {
            Status::Fleeting
        } else {
            Status::Permanent
        };
        let title = extract_title(&self.body).map(|(t, _)| t);
        let mut tags: BTreeSet<Tag> = self.fm.tags().into_iter().collect();
        tags.extend(extract_inline_tags(&self.body));
        NoteMeta {
            id,
            rel_path: rel_path.to_owned(),
            title,
            first_line: first_line(&self.body),
            status: self.fm.status().unwrap_or(path_default),
            kind: self.fm.kind(),
            source: self.fm.source(),
            created: self.fm.created().unwrap_or(fs_modified),
            modified: self.fm.modified().unwrap_or(fs_modified),
            tags,
            links_out: extract_links(&self.body),
            word_count: word_count(&self.body),
        }
    }
}

/// First non-empty line, trimmed, leading heading marks stripped.
fn first_line(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.trim_start_matches('#').trim_start().to_owned())
        .unwrap_or_default()
}

/// Iterate body lines with their byte offsets, tracking fenced-code state.
/// `f(line, line_start_offset, in_fence)`.
fn for_each_line(body: &str, mut f: impl FnMut(&str, usize, bool)) {
    let mut offset = 0;
    let mut in_fence = false;
    for line in body.split_inclusive('\n') {
        let c = line.trim_end_matches('\n').trim_end_matches('\r');
        if c.trim_start().starts_with("```") {
            in_fence = !in_fence;
            f(c, offset, true); // fence marker lines themselves are "code"
        } else {
            f(c, offset, in_fence);
        }
        offset += line.len();
    }
}

/// Byte ranges of inline-code spans (`...`) within one line.
fn inline_code_ranges(line: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut open: Option<usize> = None;
    for (i, ch) in line.char_indices() {
        if ch == '`' {
            match open.take() {
                Some(start) => ranges.push(start..i + 1),
                None => open = Some(i),
            }
        }
    }
    ranges
}

fn in_ranges(pos: usize, ranges: &[Range<usize>]) -> bool {
    ranges.iter().any(|r| r.contains(&pos))
}

pub fn extract_title(body: &str) -> Option<(String, Range<usize>)> {
    let mut found = None;
    for_each_line(body, |line, offset, in_fence| {
        if found.is_none() && !in_fence {
            if let Some(rest) = line.strip_prefix("# ") {
                let text = rest.trim();
                if !text.is_empty() {
                    let start = offset + (line.len() - rest.len()) + (rest.len() - rest.trim_start().len());
                    found = Some((text.to_owned(), start..start + text.len()));
                }
            }
        }
    });
    found
}

pub fn extract_links(body: &str) -> Vec<LinkRef> {
    let mut links = Vec::new();
    for_each_line(body, |line, offset, in_fence| {
        if in_fence {
            return;
        }
        let code = inline_code_ranges(line);
        let mut at = 0;
        while let Some(open) = line[at..].find("[[") {
            let open = at + open;
            let Some(close) = line[open + 2..].find("]]") else { break };
            let close = open + 2 + close;
            at = close + 2;
            if in_ranges(open, &code) {
                continue;
            }
            let inner = &line[open + 2..close];
            let (target, display) = match inner.split_once('|') {
                Some((t, d)) => (t.trim(), Some(d.trim().to_owned())),
                None => (inner.trim(), None),
            };
            if target.is_empty() {
                continue;
            }
            links.push(LinkRef {
                target: target.to_owned(),
                display,
                span: offset + open..offset + close + 2,
            });
        }
    });
    links
}

pub fn extract_inline_tags(body: &str) -> Vec<Tag> {
    let mut tags = Vec::new();
    for_each_line(body, |line, _offset, in_fence| {
        if in_fence {
            return;
        }
        let code = inline_code_ranges(line);
        let mut prev: Option<char> = None;
        let mut chars = line.char_indices().peekable();
        while let Some((i, ch)) = chars.next() {
            if ch == '#'
                && prev.is_none_or(char::is_whitespace)
                && !in_ranges(i, &code)
                && chars.peek().is_some_and(|(_, c)| c.is_alphanumeric())
            {
                let start = i + 1;
                let mut end = start;
                while let Some(&(j, c)) = chars.peek() {
                    if c.is_alphanumeric() || c == '-' || c == '_' {
                        end = j + c.len_utf8();
                        chars.next();
                    } else {
                        break;
                    }
                }
                if let Some(t) = Tag::new(&line[start..end]) {
                    tags.push(t);
                }
                prev = Some('x'); // non-whitespace
                continue;
            }
            prev = Some(ch);
        }
    });
    tags
}

/// Maximal alphanumeric runs.
pub fn word_count(body: &str) -> u32 {
    let mut count = 0u32;
    let mut in_word = false;
    for ch in body.chars() {
        if ch.is_alphanumeric() {
            if !in_word {
                count += 1;
                in_word = true;
            }
        } else {
            in_word = false;
        }
    }
    count
}
```

Add `pub mod doc;` to `lib.rs`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p jd-core doc`
Expected: 9 passed. If `extract_title`'s span arithmetic disagrees with the test, fix the implementation, not the test — the test's `2..24` is the byte range of the heading text after `"# "`.

- [ ] **Step 5: Commit**

```bash
git add crates/jd-core
git commit -m "feat(core): NoteDoc with body extractors"
```

---

### Task 9: Golden corpus + byte-identity integration test

**Files:**
- Create: `tests-data/golden/` (≥ 14 files, listed below)
- Create: `crates/jd-core/tests/roundtrip.rs`

**Interfaces:**
- Consumes: `NoteDoc`.
- Produces: the corpus later WPs extend (WP1d's scan tests reuse these files).

- [ ] **Step 1: Create the corpus**

Write these files with exact bytes (use `printf`/heredocs; verify with `xxd` where encoding matters):

```bash
mkdir -p tests-data/golden
cd tests-data/golden

# 01: canonical app-authored note (the spec example)
cat > 01-canonical.md << 'EOF'
---
id: 01J8ZQ4KF3T9M2X7C5VBNAE8RD
created: 2026-07-03T10:22:00Z
modified: 2026-07-04T09:10:00Z
status: permanent
kind: literature
source: "Ahrens, How to Take Smart Notes (2017)"
tags: [zettelkasten, method]
---

# Elaboration is what turns a note into knowledge

Body with [[Wiki Links]] and #inline-tags.
EOF

# 02: Obsidian-style frontmatter (unknown keys, aliases, block-list tags)
cat > 02-obsidian.md << 'EOF'
---
aliases:
  - "Alt Name"
cssclass: wide
tags:
  - imported
  - from-obsidian
publish: false
---
# Imported from Obsidian

Content with a [regular link](https://example.com).
EOF

# 03: no frontmatter at all
printf '# Bare note\n\nNo frontmatter here.\n' > 03-no-frontmatter.md

# 04: CRLF line endings throughout
printf -- '---\r\nid: 01J8ZQ4KF3T9M2X7C5VBNAE8RE\r\nstatus: fleeting\r\n---\r\nA Windows-authored scrap.\r\n' > 04-crlf.md

# 05: UTF-8 BOM before the frontmatter (whole file becomes body — still round-trips)
printf '\xef\xbb\xbf---\nstatus: fleeting\n---\nBOM file.\n' > 05-bom.md

# 06: emoji + CJK title and body
cat > 06-unicode.md << 'EOF'
---
status: permanent
---
# 🦀 Rust のメモ

対応は完璧である必要があります。Emoji in body: 🎉 #日本語
EOF

# 07: RTL body text
cat > 07-rtl.md << 'EOF'
---
status: permanent
---
# Arabic note

النص العربي يعمل من اليمين إلى اليسار [[رابط]]
EOF

# 08: weird-but-legal YAML — quoting styles, spacing, comments
cat > 08-weird-yaml.md << 'EOF'
---
id:    01J8ZQ4KF3T9M2X7C5VBNAE8RF
status:	fleeting
source: 'single quotes'
# a comment line inside frontmatter
empty-value:
deeply:
  nested:
    thing: kept verbatim
---
Body.
EOF

# 09: duplicate keys (first wins on read; both preserved)
cat > 09-duplicate-keys.md << 'EOF'
---
status: permanent
status: fleeting
---
Duplicate status lines.
EOF

# 10: frontmatter only, no body
printf -- '---\nstatus: fleeting\n---\n' > 10-frontmatter-only.md

# 11: empty file
: > 11-empty.md

# 12: unterminated frontmatter (whole file is body)
printf -- '---\nstatus: fleeting\nnever closed\n' > 12-unterminated.md

# 13: markdown constructs outside the dialect (tables, footnotes, HTML)
cat > 13-foreign-markdown.md << 'EOF'
---
status: permanent
---
# Table note

| a | b |
|---|---|
| 1 | 2 |

Footnote[^1] and <div>html</div> and $math$.

[^1]: preserved untouched.
EOF

# 14: code fences with tricky content
cat > 14-code-fences.md << 'EOF'
---
status: permanent
---
# Code

```rust
let s = "[[not a link]] #not-a-tag";
# not a heading either
```

Trailing text with [[Real Link]].
EOF
```

- [ ] **Step 2: Write the failing test**

`crates/jd-core/tests/roundtrip.rs`:

```rust
//! The load-bearing invariant (spec §13): parse → serialize is byte-identical
//! for everything not deliberately changed.

use jd_core::doc::NoteDoc;

fn golden_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests-data/golden")
}

#[test]
fn golden_corpus_round_trips_byte_identical() {
    let mut checked = 0;
    for entry in std::fs::read_dir(golden_dir()).expect("tests-data/golden exists") {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "md") {
            continue;
        }
        let src = std::fs::read_to_string(&path).unwrap();
        let doc = NoteDoc::parse(&src);
        assert_eq!(doc.serialize(), src, "byte-identity failed for {}", path.display());
        checked += 1;
    }
    assert!(checked >= 14, "corpus has {checked} files; expected at least 14");
}

#[test]
fn golden_corpus_extracts_sane_metadata() {
    // spot-checks that parsing (not just round-tripping) works on foreign files
    let src = std::fs::read_to_string(golden_dir().join("02-obsidian.md")).unwrap();
    let doc = NoteDoc::parse(&src);
    let tags: Vec<String> = doc.fm.tags().iter().map(|t| t.as_str().to_owned()).collect();
    assert_eq!(tags, vec!["imported", "from-obsidian"]);

    let src = std::fs::read_to_string(golden_dir().join("09-duplicate-keys.md")).unwrap();
    let doc = NoteDoc::parse(&src);
    assert_eq!(doc.fm.status(), Some(jd_core::note::Status::Permanent), "first key wins");
}
```

- [ ] **Step 3: Run**

Run: `cargo test -p jd-core --test roundtrip`
Expected: PASS if Tasks 6–8 are correct. Any failure here is a real parser bug — fix the parser, never weaken the corpus. (If `05-bom.md` fails: the BOM must make `FrontmatterDoc::parse` return `NoOpeningMarker`, routing the whole file to body.)

- [ ] **Step 4: Commit**

```bash
git add tests-data crates/jd-core/tests
git commit -m "test(core): golden corpus with byte-identity round-trip"
```

---

### Task 10: Randomized round-trip generator

**Files:**
- Create: `crates/jd-core/tests/randomized.rs`

**Interfaces:**
- Consumes: `Xorshift128`, `NoteDoc`.
- Produces: the adversarial-document generator (`gen_document`) that WP1b's lexer-sanity tests will import by copy (test files can't share code across crates without a helper crate — duplicate the ~60 lines there rather than adding one; note this in WP1b's plan).

- [ ] **Step 1: Write the test**

```rust
//! Randomized round-trip: 1000 adversarial documents, fixed seed (spec §13).

use jd_core::doc::NoteDoc;
use jd_core::rng::Xorshift128;

const KNOWN_KEYS: &[&str] = &["id", "created", "modified", "status", "kind", "source", "tags"];
const UNKNOWN_KEYS: &[&str] = &["aliases", "x-custom", "publish", "weird_key", "UPPER"];
const VALUES: &[&str] = &[
    "plain", "\"double quoted\"", "'single'", "[a, b, c]", "", "  spaced  ",
    "01J8ZQ4KF3T9M2X7C5VBNAE8RD", "2026-07-03T10:22:00Z", "fleeting", "with: colon",
];
const BODY_FRAGMENTS: &[&str] = &[
    "# Heading\n", "## Sub\n", "plain text line\n", "", "\n",
    "[[Link]]\n", "[[Link|display]]\n", "[[unclosed\n", "#tag mid #tag-two\n",
    "```\ncode [[x]] #y\n```\n", "`inline [[z]]`\n", "- list item\n", "1. numbered\n",
    "> quote\n", "**bold** *it* ~~strike~~\n", "日本語テキスト 🎉\n",
    "نص عربي\n", "| a | b |\n", "trailing spaces   \n", "\t\ttabs\n",
    "--- \n", "----\n", "---x\n",
];

fn pick<'a>(rng: &mut Xorshift128, pool: &[&'a str]) -> &'a str {
    pool[rng.gen_range(0..pool.len() as u64) as usize]
}

fn gen_document(rng: &mut Xorshift128) -> String {
    let mut out = String::new();
    let crlf = rng.gen_range(0..4) == 0;
    let term = if crlf { "\r\n" } else { "\n" };
    if rng.gen_range(0..5) > 0 {
        // 80%: with frontmatter
        out.push_str("---");
        out.push_str(term);
        for _ in 0..rng.gen_range(0..8) {
            let key = if rng.gen_range(0..2) == 0 {
                pick(rng, KNOWN_KEYS)
            } else {
                pick(rng, UNKNOWN_KEYS)
            };
            out.push_str(key);
            out.push_str(": ");
            out.push_str(pick(rng, VALUES));
            out.push_str(term);
            if rng.gen_range(0..6) == 0 {
                out.push_str("  - block item");
                out.push_str(term);
            }
        }
        if rng.gen_range(0..10) > 0 {
            // 10%: leave the block unterminated
            out.push_str("---");
            out.push_str(term);
        }
    }
    for _ in 0..rng.gen_range(0..20) {
        // body fragments use \n even in crlf mode — mixed line endings are a
        // deliberately adversarial case and must still round-trip
        out.push_str(pick(rng, BODY_FRAGMENTS));
    }
    out
}

#[test]
fn randomized_documents_round_trip() {
    let mut rng = Xorshift128::new(0x_5EED_CAFE);
    for i in 0..1000 {
        let doc_src = gen_document(&mut rng);
        let doc = NoteDoc::parse(&doc_src);
        assert_eq!(
            doc.serialize(),
            doc_src,
            "round-trip failed on iteration {i}; input: {doc_src:?}"
        );
    }
}

#[test]
fn randomized_metadata_extraction_never_panics() {
    let mut rng = Xorshift128::new(0x_0DDB_A11);
    let id = jd_core::id::NoteId([1; 16]);
    let t = jd_core::time::Timestamp(0);
    for _ in 0..1000 {
        let doc_src = gen_document(&mut rng);
        let doc = NoteDoc::parse(&doc_src);
        let _ = doc.to_meta(id, std::path::Path::new("notes/x.md"), t);
    }
}
```

- [ ] **Step 2: Run**

Run: `cargo test -p jd-core --test randomized`
Expected: PASS. On failure the assert prints the exact input — minimize it by hand, add it to the golden corpus as a new numbered file, fix the parser, and keep both tests.

- [ ] **Step 3: Full-workspace gate, then commit**

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/jd-core
git commit -m "test(core): randomized round-trip generator"
```

---

## Self-Review Notes

- Spec §2 field semantics: covered by Tasks 5–8 (id/status/kind/source/tags, unknown-key preservation, `#inline-tags` union).
- Spec §13 "parsers" bullet: golden corpus (Task 9) + randomized round-trips (Task 10). Lexer-span sanity belongs to WP1b.
- Deviations from the architecture doc made here (doc updated in the same session this plan was written): `FrontmatterDoc::status()` returns `Option<Status>` (caller applies the path default), `source()` returns `Option<String>`, `to_meta` takes `id` as a parameter.
- Filename machinery (`sanitize_filename`, collision suffix) is **WP1d**, not here — Task 3's `short()` just provides the suffix string.

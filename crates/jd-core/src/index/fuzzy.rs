//! Fuzzy title scorer for the palette (spec §7): fzf-style, with a distinct
//! acronym tier. Written in-house per Appendix B. Ordering contract: lower
//! tier is better; within a tier, higher score is better — sort by
//! (tier, Reverse(score)).

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum FuzzyTier {
    Exact,
    Prefix,
    Acronym,
    Subsequence,
}

#[derive(Clone, PartialEq, Debug)]
pub struct FuzzyScore {
    pub tier: FuzzyTier,
    pub score: i32,
    /// Candidate CHAR indices that matched (for highlight rendering).
    pub matched: Vec<usize>,
}

const WORD_BOUNDARY_BONUS: i32 = 10;
const CONSECUTIVE_BONUS: i32 = 8;
const GAP_PENALTY_CAP: i32 = 10;

pub fn fuzzy_match(query: &str, candidate: &str) -> Option<FuzzyScore> {
    if query.is_empty() {
        return None;
    }
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();

    // Candidate chars, lowercased 1:1 (multi-char lowercase expansions keep
    // only the first char — an accepted approximation for titles).
    let mut chars: Vec<char> = Vec::new();
    let mut word_start: Vec<bool> = Vec::new();
    let mut prev: Option<char> = None;
    for ch in candidate.chars() {
        chars.push(ch.to_lowercase().next().unwrap_or(ch));
        word_start.push(ch.is_alphanumeric() && prev.is_none_or(|p| !p.is_alphanumeric()));
        prev = Some(ch);
    }
    if q.len() > chars.len() {
        return None;
    }

    if chars.len() == q.len() && chars == q {
        return Some(FuzzyScore {
            tier: FuzzyTier::Exact,
            score: i32::MAX,
            matched: (0..q.len()).collect(),
        });
    }
    if chars[..q.len()] == q[..] {
        let score = (q.len() as i32) * 100 / chars.len() as i32;
        return Some(FuzzyScore {
            tier: FuzzyTier::Prefix,
            score,
            matched: (0..q.len()).collect(),
        });
    }

    // Acronym: every query char consumed at a word start, in order.
    let mut matched = Vec::with_capacity(q.len());
    let mut qi = 0;
    for (i, (&ch, &ws)) in chars.iter().zip(&word_start).enumerate() {
        if qi < q.len() && ws && ch == q[qi] {
            matched.push(i);
            qi += 1;
        }
    }
    if qi == q.len() {
        // earlier completion = tighter acronym = better
        let score = -(matched[q.len() - 1] as i32);
        return Some(FuzzyScore {
            tier: FuzzyTier::Acronym,
            score,
            matched,
        });
    }

    // In-order subsequence (greedy), scored by boundary/consecutive bonuses
    // minus capped gap penalties and start distance.
    let mut matched = Vec::with_capacity(q.len());
    let mut qi = 0;
    for (i, &ch) in chars.iter().enumerate() {
        if qi < q.len() && ch == q[qi] {
            matched.push(i);
            qi += 1;
        }
    }
    if qi < q.len() {
        return None;
    }
    let mut score = 0i32;
    for (k, &i) in matched.iter().enumerate() {
        if word_start[i] {
            score += WORD_BOUNDARY_BONUS;
        }
        if k > 0 {
            let gap = (i - matched[k - 1] - 1) as i32;
            if gap == 0 {
                score += CONSECUTIVE_BONUS;
            } else {
                score -= gap.min(GAP_PENALTY_CAP);
            }
        }
    }
    score -= matched[0] as i32;
    Some(FuzzyScore {
        tier: FuzzyTier::Subsequence,
        score,
        matched,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tier(q: &str, c: &str) -> FuzzyTier {
        fuzzy_match(q, c).expect("should match").tier
    }

    #[test]
    fn tiers() {
        assert_eq!(tier("egui", "egui"), FuzzyTier::Exact);
        assert_eq!(tier("Egui", "egui"), FuzzyTier::Exact); // case-insensitive
        assert_eq!(tier("egui", "egui tradeoffs"), FuzzyTier::Prefix);
        assert_eq!(
            tier("nasa", "National Aeronautics and Space Administration"),
            FuzzyTier::Acronym
        );
        assert_eq!(tier("etf", "egui tradeoffs"), FuzzyTier::Subsequence);
    }

    #[test]
    fn tier_ordering_is_the_ranking() {
        assert!(FuzzyTier::Exact < FuzzyTier::Prefix);
        assert!(FuzzyTier::Prefix < FuzzyTier::Acronym);
        assert!(FuzzyTier::Acronym < FuzzyTier::Subsequence);
    }

    #[test]
    fn acronym_beats_subsequence_ranking_table() {
        // The spec's pinned example: 'nasa' must rank the acronym candidate
        // above candidates that merely contain the letters in order.
        let a = fuzzy_match("nasa", "National Aeronautics and Space Administration").unwrap();
        assert_eq!(a.tier, FuzzyTier::Acronym);
        // a true subsequence candidate (word initials f/n/a — NOT an acronym for nasa):
        let c = fuzzy_match("nasa", "front nasal anatomy").unwrap();
        assert_eq!(c.tier, FuzzyTier::Subsequence);
        assert!(a.tier < c.tier);
        // and a prefix candidate outranks the acronym
        let b = fuzzy_match("nasa", "nasal decongestants and their history").unwrap();
        assert_eq!(b.tier, FuzzyTier::Prefix);
        assert!(b.tier < a.tier);
    }

    #[test]
    fn consecutive_run_beats_scattered() {
        let tight = fuzzy_match("abc", "xx abcdef").unwrap();
        let scattered = fuzzy_match("abc", "xx a1b2c3").unwrap();
        assert_eq!(tight.tier, FuzzyTier::Subsequence);
        assert_eq!(scattered.tier, FuzzyTier::Subsequence);
        assert!(
            tight.score > scattered.score,
            "consecutive bonus must dominate"
        );
    }

    #[test]
    fn word_boundary_hits_score_higher() {
        let boundary = fuzzy_match("st", "note structure").unwrap();
        let inside = fuzzy_match("st", "faster").unwrap();
        assert_eq!(boundary.tier, FuzzyTier::Subsequence);
        assert_eq!(inside.tier, FuzzyTier::Subsequence);
        assert!(boundary.score > inside.score);
    }

    #[test]
    fn matched_indices_are_char_indices() {
        let m = fuzzy_match("nasa", "National Aeronautics and Space Administration").unwrap();
        assert_eq!(m.matched, vec![0, 9, 25, 31]); // N, A(eronautics), S(pace), A(dministration)
        let m = fuzzy_match("ab", "áxab").unwrap(); // multibyte before the match
        assert_eq!(m.matched, vec![2, 3]); // CHAR indices, not bytes
    }

    #[test]
    fn no_match_and_edge_cases() {
        assert!(fuzzy_match("xyz", "egui tradeoffs").is_none());
        assert!(fuzzy_match("", "anything").is_none());
        assert!(fuzzy_match("longer", "log").is_none()); // query longer than candidate
    }
}

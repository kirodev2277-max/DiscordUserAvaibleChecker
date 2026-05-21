//! 3- and 4-letter Discord-handle generators.
//!
//! Discord handles have many shapes, but short letter-only handles are
//! what hunters actually care about. This generator is intentionally
//! restricted to [`Length::Three`] and [`Length::Four`], either over
//! lowercase letters only or letters + digits.
//!
//! Two modes:
//!
//! - [`GenMode::All`]: lexicographic exhaustive iteration (deterministic).
//! - [`GenMode::Random`]: uniform unique sampling with an optional seed.

use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use std::collections::HashSet;

/// Pre-computed search-space size for 3 letters: 26³.
pub const SPACE_3_LETTERS: usize = 26 * 26 * 26;
/// Pre-computed search-space size for 4 letters: 26⁴.
pub const SPACE_4_LETTERS: usize = 26 * 26 * 26 * 26;

/// Length of the handle to generate. Restricted to 3 or 4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Length {
    Three,
    Four,
}

impl Length {
    pub fn as_usize(self) -> usize {
        match self {
            Length::Three => 3,
            Length::Four => 4,
        }
    }

    pub fn from_u32(value: u32) -> Option<Self> {
        match value {
            3 => Some(Length::Three),
            4 => Some(Length::Four),
            _ => None,
        }
    }
}

/// Character pool the generator draws from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charset {
    /// `a-z` (26 chars).
    Letters,
    /// `a-z` + `0-9` (36 chars).
    Alnum,
}

impl Charset {
    pub fn chars(self) -> &'static [u8] {
        match self {
            Charset::Letters => b"abcdefghijklmnopqrstuvwxyz",
            Charset::Alnum => b"abcdefghijklmnopqrstuvwxyz0123456789",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Charset::Letters => "letters",
            Charset::Alnum => "alnum",
        }
    }
}

/// Generation strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenMode {
    All,
    Random,
}

/// Aggregated config consumed by [`generate`].
#[derive(Debug, Clone)]
pub struct GenerateConfig {
    pub length: Length,
    pub charset: Charset,
    pub mode: GenMode,
    /// Number of items to produce. In `All` mode this caps the iteration;
    /// `None` means "emit the entire space". In `Random` mode this is
    /// the requested unique sample size.
    pub count: Option<usize>,
    /// Optional RNG seed for `Random` mode reproducibility.
    pub seed: Option<u64>,
}

impl GenerateConfig {
    pub fn space_size(&self) -> usize {
        let base = self.charset.chars().len();
        match self.length {
            Length::Three => base * base * base,
            Length::Four => base * base * base * base,
        }
    }
}

/// Materialize the requested batch.
///
/// `All` returns a stable lexicographic ordering. `Random` returns
/// uniformly-distributed unique strings; the seed (if provided) makes
/// output deterministic.
pub fn generate(cfg: &GenerateConfig) -> Vec<String> {
    let space = cfg.space_size();
    let target = match cfg.mode {
        GenMode::All => cfg.count.unwrap_or(space).min(space),
        GenMode::Random => cfg.count.unwrap_or(1000).min(space),
    };
    if target == 0 {
        return Vec::new();
    }

    match cfg.mode {
        GenMode::All => generate_all(cfg.charset, cfg.length, target),
        GenMode::Random => generate_random(cfg.charset, cfg.length, target, cfg.seed),
    }
}

fn generate_all(charset: Charset, length: Length, target: usize) -> Vec<String> {
    let chars = charset.chars();
    let base = chars.len();
    let mut out = Vec::with_capacity(target);
    let n = length.as_usize();
    // Iterate via a digit counter in base `base`.
    let mut idx = 0usize;
    let total = match length {
        Length::Three => base.saturating_pow(3),
        Length::Four => base.saturating_pow(4),
    };
    while idx < total && out.len() < target {
        let mut buf = [0u8; 4];
        let slice = &mut buf[..n];
        let mut v = idx;
        for i in (0..n).rev() {
            slice[i] = chars[v % base];
            v /= base;
        }
        out.push(std::str::from_utf8(slice).unwrap().to_string());
        idx += 1;
    }
    out
}

fn generate_random(
    charset: Charset,
    length: Length,
    target: usize,
    seed: Option<u64>,
) -> Vec<String> {
    let chars = charset.chars();
    let n = length.as_usize();
    let space = match length {
        Length::Three => chars.len().saturating_pow(3),
        Length::Four => chars.len().saturating_pow(4),
    };
    let target = target.min(space);

    let mut rng: StdRng = match seed {
        Some(s) => StdRng::seed_from_u64(s),
        None => StdRng::from_entropy(),
    };

    // If the request is a large fraction of the search space, do a
    // partial Fisher-Yates over indices rather than rejection sampling
    // — much faster and avoids unbounded retries.
    if target * 2 >= space {
        let mut pool: Vec<usize> = (0..space).collect();
        let mut out = Vec::with_capacity(target);
        for i in 0..target {
            let j = rng.gen_range(i..space);
            pool.swap(i, j);
            out.push(index_to_handle(pool[i], chars, n));
        }
        out.shuffle(&mut rng);
        return out;
    }

    let mut seen: HashSet<usize> = HashSet::with_capacity(target);
    let mut out = Vec::with_capacity(target);
    while out.len() < target {
        let idx = rng.gen_range(0..space);
        if seen.insert(idx) {
            out.push(index_to_handle(idx, chars, n));
        }
    }
    out
}

fn index_to_handle(mut idx: usize, chars: &[u8], n: usize) -> String {
    let base = chars.len();
    let mut buf = [0u8; 4];
    let slice = &mut buf[..n];
    for i in (0..n).rev() {
        slice[i] = chars[idx % base];
        idx /= base;
    }
    std::str::from_utf8(slice).unwrap().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn all_three_letters_is_exact_size() {
        let cfg = GenerateConfig {
            length: Length::Three,
            charset: Charset::Letters,
            mode: GenMode::All,
            count: None,
            seed: None,
        };
        let out = generate(&cfg);
        assert_eq!(out.len(), SPACE_3_LETTERS);
        // First and last entries (lex order).
        assert_eq!(out.first().map(String::as_str), Some("aaa"));
        assert_eq!(out.last().map(String::as_str), Some("zzz"));
        // No duplicates.
        let set: HashSet<&String> = out.iter().collect();
        assert_eq!(set.len(), out.len());
    }

    #[test]
    fn all_four_letters_is_full_space() {
        let cfg = GenerateConfig {
            length: Length::Four,
            charset: Charset::Letters,
            mode: GenMode::All,
            count: Some(10),
            seed: None,
        };
        let out = generate(&cfg);
        assert_eq!(out.len(), 10);
        assert_eq!(out.first().map(String::as_str), Some("aaaa"));
    }

    #[test]
    fn random_is_unique_and_correct_length() {
        let cfg = GenerateConfig {
            length: Length::Four,
            charset: Charset::Alnum,
            mode: GenMode::Random,
            count: Some(500),
            seed: Some(42),
        };
        let out = generate(&cfg);
        assert_eq!(out.len(), 500);
        for handle in &out {
            assert_eq!(handle.len(), 4);
            assert!(handle
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
        }
        let set: HashSet<&String> = out.iter().collect();
        assert_eq!(set.len(), out.len());
    }

    #[test]
    fn random_seed_is_reproducible() {
        let mk = || GenerateConfig {
            length: Length::Three,
            charset: Charset::Letters,
            mode: GenMode::Random,
            count: Some(50),
            seed: Some(7),
        };
        assert_eq!(generate(&mk()), generate(&mk()));
    }

    #[test]
    fn random_cap_at_space_size() {
        let cfg = GenerateConfig {
            length: Length::Three,
            charset: Charset::Letters,
            mode: GenMode::Random,
            count: Some(SPACE_3_LETTERS + 5000),
            seed: Some(1),
        };
        let out = generate(&cfg);
        assert_eq!(out.len(), SPACE_3_LETTERS);
        let set: HashSet<&String> = out.iter().collect();
        assert_eq!(set.len(), SPACE_3_LETTERS);
    }
}

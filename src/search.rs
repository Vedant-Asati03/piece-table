use memchr::memchr_iter;

use super::{BufferKind, PieceTable, SearchAlgorithm};

impl<T: Clone + PartialEq> PieceTable<T> {
    pub fn find_substring(&self, pattern: &[T]) -> Vec<usize> {
        if pattern.is_empty() {
            return vec![];
        }

        let lps = build_kmp_lps(pattern);
        let mut out = Vec::new();
        let mut matched = 0usize;
        let mut index = 0usize;

        for item in self.iter() {
            while matched > 0 && *item != pattern[matched] {
                matched = lps[matched - 1];
            }

            if *item == pattern[matched] {
                matched += 1;
                if matched == pattern.len() {
                    out.push(index + 1 - pattern.len());
                    matched = lps[matched - 1];
                }
            }

            index += 1;
        }

        out
    }
}

impl PieceTable<u8> {
    pub fn find_substring_with(&self, pattern: &[u8], algorithm: SearchAlgorithm) -> Vec<usize> {
        if pattern.is_empty() {
            return vec![];
        }

        match algorithm {
            SearchAlgorithm::Kmp => self.find_substring(pattern),
            SearchAlgorithm::BoyerMooreHorspool => find_bmh_bytes(self, pattern),
            SearchAlgorithm::Auto => {
                if pattern.len() == 1 {
                    self.find_substring_optimized(pattern)
                } else if pattern.len() >= 8 {
                    find_bmh_bytes(self, pattern)
                } else {
                    self.find_substring(pattern)
                }
            }
        }
    }

    pub fn find_substring_optimized(&self, pattern: &[u8]) -> Vec<usize> {
        if pattern.is_empty() {
            return vec![];
        }

        if pattern.len() == 1 {
            let mut out = Vec::new();
            let needle = pattern[0];

            self.for_each_piece_in_order(|piece, piece_start| {
                let slice = match piece.buffer {
                    BufferKind::File => &self.original[piece.start..piece.end],
                    BufferKind::Add => &self.add[piece.start..piece.end],
                };

                for idx in memchr_iter(needle, slice) {
                    out.push(piece_start + idx);
                }
            });

            return out;
        }

        self.find_substring_with(pattern, SearchAlgorithm::Auto)
    }

    pub fn find_regex_bytes(&self, pattern: &str) -> Result<Vec<(usize, usize)>, regex::Error> {
        let re = regex::bytes::Regex::new(pattern)?;
        let mut bytes = Vec::with_capacity(self.len());
        for b in self.iter() {
            bytes.push(*b);
        }

        Ok(re.find_iter(&bytes).map(|m| (m.start(), m.end())).collect())
    }
}

fn find_bmh_bytes(table: &PieceTable<u8>, pattern: &[u8]) -> Vec<usize> {
    let n = table.len();
    let m = pattern.len();
    if m == 0 || m > n {
        return vec![];
    }

    let mut text = Vec::with_capacity(n);
    for b in table.iter() {
        text.push(*b);
    }

    let mut shift = [m; 256];
    if m > 1 {
        for (i, &byte) in pattern.iter().take(m - 1).enumerate() {
            shift[byte as usize] = m - 1 - i;
        }
    }

    let mut out = Vec::new();
    let mut i = 0usize;

    while i + m <= n {
        let mut j = m;
        while j > 0 && pattern[j - 1] == text[i + j - 1] {
            j -= 1;
        }

        if j == 0 {
            out.push(i);
            i += 1;
        } else {
            let c = text[i + m - 1] as usize;
            i += shift[c].max(1);
        }
    }

    out
}

pub(crate) fn build_kmp_lps<T: PartialEq>(pattern: &[T]) -> Vec<usize> {
    let mut lps = vec![0; pattern.len()];
    let mut len = 0usize;
    let mut i = 1usize;

    while i < pattern.len() {
        if pattern[i] == pattern[len] {
            len += 1;
            lps[i] = len;
            i += 1;
        } else if len != 0 {
            len = lps[len - 1];
        } else {
            lps[i] = 0;
            i += 1;
        }
    }

    lps
}

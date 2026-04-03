use super::{
    CompactPieceTable, INTERNAL_MAX_CHILDREN, INTERNAL_MIN_CHILDREN, LEAF_MAX_PIECES,
    LEAF_MIN_PIECES, PieceTable, SearchAlgorithm, TextBuffer,
};
use crate::tree::BNode;

fn assert_bplus_occupancy(node: &BNode, is_root: bool) {
    match node {
        BNode::Leaf { pieces, .. } => {
            if !is_root {
                assert!(pieces.len() >= LEAF_MIN_PIECES || pieces.is_empty());
            }
            assert!(pieces.len() <= LEAF_MAX_PIECES);
        }
        BNode::Internal { children, .. } => {
            if !is_root {
                assert!(children.len() >= INTERNAL_MIN_CHILDREN || children.is_empty());
            }
            assert!(children.len() <= INTERNAL_MAX_CHILDREN);
            for child in children {
                assert_bplus_occupancy(child.as_ref(), false);
            }
        }
    }
}

fn next_seed(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    *seed
}

fn collect(table: &PieceTable<char>) -> String {
    table.iter().copied().collect()
}

#[test]
fn basic_ops() {
    let mut t = PieceTable::new("hello".chars().collect());
    t.insert(5, &[' ', 'w', 'o', 'r', 'l', 'd']);
    assert_eq!(collect(&t), "hello world");

    t.delete(5, 1);
    assert_eq!(collect(&t), "helloworld");
    assert!(t.validate().is_ok());
}

#[test]
fn cursor_locality_walk() {
    let mut t = PieceTable::new("abcde".chars().collect());
    t.insert(2, &['X', 'Y']);

    let mut c = t.cursor(2);
    assert_eq!(c.get(), Some(&'X'));
    assert_eq!(c.next(), Some(&'Y'));
    assert_eq!(c.next(), Some(&'c'));
    assert_eq!(c.prev(), Some(&'Y'));
}

#[test]
fn range_iter_works() {
    let t = PieceTable::new("abcdefghij".chars().collect());
    let out: String = t.range(2, 7).copied().collect();
    assert_eq!(out, "cdefg");
}

#[test]
fn get_across_boundaries() {
    let mut t = PieceTable::new("abcd".chars().collect());
    t.insert(2, &['X', 'Y', 'Z']);
    assert_eq!(t.get(0), Some(&'a'));
    assert_eq!(t.get(2), Some(&'X'));
    assert_eq!(t.get(4), Some(&'Z'));
    assert_eq!(t.get(5), Some(&'c'));
    assert_eq!(t.get(t.len()), None);
}

#[test]
fn kmp_search_across_pieces() {
    let mut t = PieceTable::new("ababa".chars().collect());
    t.insert(5, &['b', 'a']);
    let hits = t.find_substring(&['a', 'b', 'a']);
    assert_eq!(hits, vec![0, 2, 4]);
}

#[test]
fn coalescing_adjacent_add_pieces() {
    let mut t = PieceTable::new(Vec::<char>::new());
    t.insert(0, &['a']);
    t.insert(1, &['b']);
    t.insert(2, &['c']);

    assert_eq!(collect(&t), "abc");
    assert!(t.validate().is_ok());
}

#[test]
fn large_doc_100k_to_3m() {
    let sizes = [100_000usize, 500_000, 1_000_000, 2_000_000, 3_000_000];
    for size in sizes {
        let mut t = PieceTable::new(vec!['a'; size]);
        t.insert(size / 2, &['x'; 128]);
        t.delete(size / 3, 64);
        assert_eq!(t.len(), size + 64);
        assert!(t.validate().is_ok());
    }
}

#[test]
fn sequential_insert_fast_path_behaves() {
    let mut t = PieceTable::new(Vec::<char>::new());
    for ch in "abcdefg".chars() {
        let pos = t.len();
        t.insert(pos, &[ch]);
    }

    assert_eq!(collect(&t), "abcdefg");
    assert!(t.validate().is_ok());
    assert_eq!(t.stats().split_calls, 0);
}

#[test]
fn get_locality_cache_consistency() {
    let mut t = PieceTable::new("hello world".chars().collect());
    t.insert(5, &['X', 'Y', 'Z']);

    let probe = [0usize, 1, 2, 5, 6, 7, 8, 9, t.len() - 1];
    for &idx in &probe {
        let a = t.get(idx).copied();
        let b = t.get(idx).copied();
        assert_eq!(a, b);
    }

    for idx in 0..t.len() {
        let a = t.get(idx).copied();
        let b = t.iter().nth(idx).copied();
        assert_eq!(a, b);
    }

    assert!(t.validate().is_ok());
}

#[test]
fn stress_mixed_edits_matches_vec_model() {
    let mut t = PieceTable::new("seed_data".chars().collect());
    let mut expected: Vec<char> = "seed_data".chars().collect();
    let mut seed = 0xDEADBEEF_u64;

    for _ in 0..2_000 {
        let r = next_seed(&mut seed);
        if expected.is_empty() || r % 3 != 0 {
            let insert_len = (r as usize % 6) + 1;
            let pos = if expected.is_empty() {
                0
            } else {
                (next_seed(&mut seed) as usize) % (expected.len() + 1)
            };

            let mut data = Vec::with_capacity(insert_len);
            for _ in 0..insert_len {
                let ch = (b'a' + (next_seed(&mut seed) % 26) as u8) as char;
                data.push(ch);
            }

            t.insert(pos, &data);
            expected.splice(pos..pos, data.into_iter());
        } else {
            let pos = (next_seed(&mut seed) as usize) % expected.len();
            let max_len = (expected.len() - pos).min(6);
            let del_len = ((next_seed(&mut seed) as usize) % max_len) + 1;

            t.delete(pos, del_len);
            expected.drain(pos..pos + del_len);
        }

        assert!(t.validate().is_ok());
        let actual: Vec<char> = t.iter().copied().collect();
        assert_eq!(actual, expected);
    }
}

#[test]
fn occupancy_after_heavy_deletes() {
    let mut t = PieceTable::new(vec!['a'; 50_000]);

    for i in 0..2_000 {
        let pos = (i * 17) % t.len().max(1);
        let max_del = (t.len().saturating_sub(pos)).min(16);
        if max_del > 0 {
            t.delete(pos, max_del);
        }
        if i % 3 == 0 {
            t.insert(t.len() / 2, &['x'; 8]);
        }
        assert!(t.validate().is_ok());
    }

    if let Some(root) = &t.root {
        assert_bplus_occupancy(root.as_ref(), true);
    }
}

#[test]
fn optimized_byte_search_matches_kmp() {
    let mut t = PieceTable::new(b"abcxabcdabxabcdabcdabcy".to_vec());
    t.insert(10, b"zzzz");

    let pattern = b"abcd";
    let kmp = t.find_substring(pattern);
    let fast = t.find_substring_optimized(pattern);
    assert_eq!(kmp, fast);

    let single = t.find_substring_optimized(b"z");
    assert_eq!(single.len(), 4);
}

#[test]
fn regex_byte_search_basic() {
    let mut t = PieceTable::new(b"foo-123 bar-999 baz-42".to_vec());
    t.insert(3, b"-777");

    let hits = t.find_regex_bytes(r"[a-z]+-\d+").unwrap();
    assert!(!hits.is_empty());

    let mut haystack = Vec::new();
    for b in t.iter() {
        haystack.push(*b);
    }

    for (s, e) in hits {
        assert!(s < e && e <= haystack.len());
    }
}

#[test]
fn bmh_matches_kmp_for_bytes() {
    let mut t = PieceTable::new(b"zzabcdxxabcdyyabcdzz".to_vec());
    t.insert(5, b"abcd");
    let pattern = b"abcd";

    let kmp = t.find_substring_with(pattern, SearchAlgorithm::Kmp);
    let bmh = t.find_substring_with(pattern, SearchAlgorithm::BoyerMooreHorspool);
    let auto = t.find_substring_with(pattern, SearchAlgorithm::Auto);
    assert_eq!(kmp, bmh);
    assert_eq!(kmp, auto);
}

#[test]
fn char_regex_search_basic() {
    let mut t = PieceTable::new("alpha-12 beta-77".chars().collect());
    t.insert(5, &['X', 'X']);

    let hits = t.find_regex(r"[a-zA-Z]+-\d+").unwrap();
    assert!(!hits.is_empty());

    let text: String = t.iter().copied().collect();
    for (s, e) in hits {
        assert!(s < e && e <= text.len());
    }
}

#[test]
fn compact_backend_basic_ops() {
    let mut t = CompactPieceTable::new("hello".chars().collect());
    t.insert(5, &[' ', 'w', 'o', 'r', 'l', 'd']);
    let out: String = t.iter().copied().collect();
    assert_eq!(out, "hello world");

    t.delete(5, 1);
    let out2: String = t.iter().copied().collect();
    assert_eq!(out2, "helloworld");
    assert!(t.validate().is_ok());
}

#[test]
fn compact_backend_parity_randomized() {
    let mut a = PieceTable::new("seed_data".chars().collect());
    let mut b = CompactPieceTable::new("seed_data".chars().collect());
    let mut seed = 0xBAD5EED_u64;

    for _ in 0..1000 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        if a.is_empty() || seed % 3 != 0 {
            let n = (seed as usize % 4) + 1;
            let pos = if a.is_empty() {
                0
            } else {
                (seed as usize) % (a.len() + 1)
            };

            let mut data = Vec::with_capacity(n);
            for j in 0..n {
                let ch = (b'a' + ((seed as usize + j) % 26) as u8) as char;
                data.push(ch);
            }

            a.insert(pos, &data);
            b.insert(pos, &data);
        } else {
            let pos = (seed as usize) % a.len();
            let max_len = (a.len() - pos).min(5);
            let len = ((seed as usize) % max_len) + 1;
            a.delete(pos, len);
            b.delete(pos, len);
        }

        let sa: String = a.iter().copied().collect();
        let sb: String = b.iter().copied().collect();
        assert_eq!(sa, sb);
        assert!(b.validate().is_ok());
    }
}

#[test]
fn grapheme_layer_basics() {
    let mut t = PieceTable::new("a\u{0301}👍🏽z".chars().collect());
    assert_eq!(t.grapheme_count(), 3);
    assert_eq!(t.grapheme_at(0).as_deref(), Some("a\u{0301}"));
    assert_eq!(t.grapheme_at(1).as_deref(), Some("👍🏽"));
    assert_eq!(t.grapheme_range(0, 2), "a\u{0301}👍🏽");

    t.insert(t.len(), &['!', '!']);
    assert_eq!(t.find_grapheme_substring("👍🏽z"), vec![1]);
    assert!(t.validate().is_ok());
}

#[test]
fn grapheme_regex_ranges() {
    let t = PieceTable::new("hi 👩‍💻 world 👩‍💻".chars().collect());
    let hits = t.find_regex_grapheme("👩‍💻").unwrap();
    assert_eq!(hits.len(), 2);
    for (s, e) in hits {
        assert!(s < e);
    }
}

#[test]
fn textbuffer_trait_parity() {
    fn exercise<TBuf: TextBuffer<char>>(buf: &mut TBuf) {
        buf.insert(0, &['a', 'b', 'c']);
        buf.insert(1, &['X']);
        buf.delete(2, 1);
        assert!(buf.get(0).is_some());
        assert!(buf.validate().is_ok());
    }

    let mut a = PieceTable::new(Vec::<char>::new());
    let mut b = CompactPieceTable::new(Vec::<char>::new());
    exercise(&mut a);
    exercise(&mut b);
}

#[test]
fn debug_check_toggle_roundtrip() {
    let mut t = PieceTable::new("abc".chars().collect());
    let default_enabled = t.debug_checks_enabled();

    t.set_debug_checks_enabled(!default_enabled);
    assert_eq!(t.debug_checks_enabled(), !default_enabled);

    t.set_debug_checks_enabled(default_enabled);
    assert_eq!(t.debug_checks_enabled(), default_enabled);
}

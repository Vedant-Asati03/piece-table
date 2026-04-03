//! Piece-table text editing buffers with two interchangeable backends.
//!
//! # Examples
//!
//! ```
//! use piece_table::PieceTable;
//!
//! let mut table = PieceTable::new("hello".chars().collect());
//! table.insert(5, &[' ', 'w', 'o', 'r', 'l', 'd']);
//! table.delete(0, 1);
//! let text: String = table.iter().copied().collect();
//! assert_eq!(text, "ello world");
//! ```
//!
//! ```
//! use piece_table::{CompactPieceTable, TextBuffer};
//!
//! let mut table = CompactPieceTable::new(Vec::<char>::new());
//! table.insert(0, &['a', 'b', 'c']);
//! assert_eq!(table.get(1), Some(&'b'));
//! assert!(table.validate().is_ok());
//! ```

use std::cell::Cell;

use unicode_segmentation::UnicodeSegmentation;

mod compact;
mod iter;
mod search;
mod tree;
pub use compact::{CompactIter, CompactPieceTable, CompactRangeIter};
pub use iter::{Cursor, Iter, RangeIter};
use search::build_kmp_lps;
use tree::{BNode, NodePool, Tree, can_coalesce};

/// Identifies which underlying immutable buffer a piece references.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufferKind {
    File,
    Add,
}

#[derive(Clone, Debug)]
struct Piece {
    buffer: BufferKind,
    start: usize,
    end: usize,
}

impl Piece {
    fn len(&self) -> usize {
        self.end - self.start
    }
}

const LEAF_MAX_PIECES: usize = 64;
const INTERNAL_MAX_CHILDREN: usize = 32;
const LEAF_MIN_PIECES: usize = LEAF_MAX_PIECES / 2;
const INTERNAL_MIN_CHILDREN: usize = INTERNAL_MAX_CHILDREN / 2;

/// Runtime counters useful for profiling edit behavior.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub split_calls: usize,
    pub merge_calls: usize,
    pub inserts: usize,
    pub deletes: usize,
    pub coalesces: usize,
}

/// Stable common API shared by editable text backends in this crate.
pub trait TextBuffer<T: Clone + PartialEq> {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool;
    fn insert(&mut self, pos: usize, data: &[T]);
    fn delete(&mut self, pos: usize, len: usize);
    fn get(&self, index: usize) -> Option<&T>;
    fn validate(&self) -> Result<(), String>;
}

/// Selects the substring search strategy for byte-oriented APIs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchAlgorithm {
    Auto,
    Kmp,
    BoyerMooreHorspool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AccessCache {
    piece_start: usize,
    piece_end: usize,
    buffer: BufferKind,
    buffer_start: usize,
}

/// Primary piece-table backend using a pooled B+ tree.
#[derive(Debug, Default)]
pub struct PieceTable<T> {
    original: Vec<T>,
    add: Vec<T>,
    root: Tree,
    total_len: usize,
    stats: Stats,
    debug_checks_enabled: bool,
    last_access: Cell<Option<AccessCache>>,
    pool: NodePool,
}

impl<T: Clone + PartialEq> PieceTable<T> {
    pub fn new(initial: Vec<T>) -> Self {
        let total_len = initial.len();
        let mut table = Self {
            original: initial,
            add: Vec::new(),
            root: None,
            total_len,
            stats: Stats::default(),
            debug_checks_enabled: cfg!(debug_assertions),
            last_access: Cell::new(None),
            pool: NodePool::default(),
        };

        if total_len > 0 {
            let mut pieces = Vec::with_capacity(LEAF_MAX_PIECES);
            pieces.push(Piece {
                buffer: BufferKind::File,
                start: 0,
                end: total_len,
            });
            table.root = Some(table.pool.alloc(BNode::Leaf {
                pieces,
                subtree_len: total_len,
            }));
        }

        table
    }

    pub fn len(&self) -> usize {
        self.total_len
    }

    pub fn is_empty(&self) -> bool {
        self.total_len == 0
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    pub fn reset_stats(&mut self) {
        self.stats = Stats::default();
    }

    pub fn set_debug_checks_enabled(&mut self, enabled: bool) {
        self.debug_checks_enabled = enabled;
    }

    pub fn debug_checks_enabled(&self) -> bool {
        self.debug_checks_enabled
    }

    fn post_mutation_debug_validate(&self, op: &str) {
        if self.debug_checks_enabled && cfg!(debug_assertions) {
            if let Err(err) = self.validate() {
                panic!("post-{op} invariant validation failed: {err}");
            }
        }
    }

    pub fn reserve_additional(&mut self, additional: usize) {
        self.add.reserve(additional);
    }

    pub fn reserve_for_edits(&mut self, estimated_insert_ops: usize, avg_insert_len: usize) {
        self.add
            .reserve(estimated_insert_ops.saturating_mul(avg_insert_len));
    }

    pub fn height(&self) -> usize {
        self.root.as_ref().map_or(0, |n| n.height())
    }

    pub fn insert(&mut self, pos: usize, data: &[T]) {
        assert!(pos <= self.total_len, "insert position out of bounds");
        if data.is_empty() {
            return;
        }

        let start = self.add.len();
        self.add.extend_from_slice(data);
        let inserted = Piece {
            buffer: BufferKind::Add,
            start,
            end: self.add.len(),
        };

        if self.root.is_none() {
            let inserted_len = inserted.len();
            let mut pieces = Vec::with_capacity(LEAF_MAX_PIECES);
            pieces.push(inserted);
            self.root = Some(self.pool.alloc(BNode::Leaf {
                subtree_len: inserted_len,
                pieces,
            }));
        } else {
            let mut root = self.root.take().unwrap();
            let mut did_extend = false;
            if pos > 0 {
                did_extend = root.try_extend_add_at_pos(
                    pos,
                    inserted.start,
                    inserted.end,
                    &mut self.stats,
                    &mut self.pool,
                );
            }

            if did_extend {
                self.root = Some(root);
            } else {
                let split = root.insert_at(pos, inserted, &mut self.stats, &mut self.pool);
                if let Some(right) = split {
                    self.stats.split_calls += 1;
                    let subtree_len = root.len() + right.len();
                    let mut children = Vec::with_capacity(INTERNAL_MAX_CHILDREN);
                    children.push(root);
                    children.push(right);
                    self.root = Some(self.pool.alloc(BNode::Internal {
                        children,
                        subtree_len,
                    }));
                } else {
                    self.root = Some(root);
                }
            }
        }

        self.total_len += data.len();
        self.stats.inserts += 1;
        self.last_access.set(None);
        self.post_mutation_debug_validate("insert");
    }

    pub fn delete(&mut self, pos: usize, len: usize) {
        assert!(pos <= self.total_len, "delete position out of bounds");
        assert!(
            len <= self.total_len.saturating_sub(pos),
            "delete range out of bounds"
        );

        if len == 0 || self.root.is_none() {
            return;
        }

        let mut root = self.root.take().unwrap();
        root.delete_range(pos, len, &mut self.stats, &mut self.pool);

        if root.len() == 0 {
            self.pool.recycle(root);
            self.root = None;
        } else {
            self.root = Some(root.compact_root(&mut self.pool));
        }

        self.total_len -= len;
        self.stats.deletes += 1;
        self.last_access.set(None);
        self.post_mutation_debug_validate("delete");
    }

    pub fn get(&self, index: usize) -> Option<&T> {
        if index >= self.total_len {
            return None;
        }

        if let Some(cache) = self.last_access.get()
            && index >= cache.piece_start
            && index < cache.piece_end
        {
            let buffer_index = cache.buffer_start + (index - cache.piece_start);
            return match cache.buffer {
                BufferKind::File => self.original.get(buffer_index),
                BufferKind::Add => self.add.get(buffer_index),
            };
        }

        let Some(root) = self.root.as_ref() else {
            return None;
        };
        let (piece, offset, piece_start) = root.find(index, 0);
        self.last_access.set(Some(AccessCache {
            piece_start,
            piece_end: piece_start + piece.len(),
            buffer: piece.buffer,
            buffer_start: piece.start,
        }));
        let buffer_index = piece.start + offset;
        match piece.buffer {
            BufferKind::File => self.original.get(buffer_index),
            BufferKind::Add => self.add.get(buffer_index),
        }
    }

    pub fn iter(&self) -> Iter<'_, T> {
        Iter::new(self)
    }

    pub fn range(&self, start: usize, end: usize) -> RangeIter<'_, T> {
        assert!(start <= end, "invalid range: start > end");
        assert!(end <= self.total_len, "range end out of bounds");
        RangeIter {
            cursor: self.cursor(start),
            remaining: end - start,
            started: false,
        }
    }

    pub fn cursor(&self, pos: usize) -> Cursor<'_, T> {
        Cursor::new(self, pos)
    }

    fn for_each_piece_in_order<F>(&self, mut visitor: F)
    where
        F: FnMut(&Piece, usize),
    {
        fn walk<F>(node: &BNode, base: usize, visitor: &mut F)
        where
            F: FnMut(&Piece, usize),
        {
            match node {
                BNode::Leaf { pieces, .. } => {
                    let mut offset = 0usize;
                    for piece in pieces {
                        visitor(piece, base + offset);
                        offset += piece.len();
                    }
                }
                BNode::Internal { children, .. } => {
                    let mut offset = 0usize;
                    for child in children {
                        walk(child.as_ref(), base + offset, visitor);
                        offset += child.len();
                    }
                }
            }
        }

        if let Some(root) = self.root.as_ref() {
            walk(root.as_ref(), 0, &mut visitor);
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        let Some(root) = &self.root else {
            if self.total_len != 0 {
                return Err("non-zero total_len with empty tree".to_string());
            }
            return Ok(());
        };

        let mut flat = Vec::new();
        let computed = root.validate_and_collect(&mut flat)?;
        if computed != self.total_len {
            return Err(format!(
                "root length mismatch: computed={}, total={}",
                computed, self.total_len
            ));
        }

        for piece in &flat {
            if piece.len() == 0 {
                return Err("zero-length piece found".to_string());
            }
            match piece.buffer {
                BufferKind::File => {
                    if piece.end > self.original.len() {
                        return Err("file piece out of bounds".to_string());
                    }
                }
                BufferKind::Add => {
                    if piece.end > self.add.len() {
                        return Err("add piece out of bounds".to_string());
                    }
                }
            }
        }

        for pair in flat.windows(2) {
            if can_coalesce(&pair[0], &pair[1]) {
                return Err("adjacent coalescible pieces found".to_string());
            }
        }

        Ok(())
    }
}

impl<T: Clone + PartialEq> TextBuffer<T> for PieceTable<T> {
    fn len(&self) -> usize {
        PieceTable::len(self)
    }

    fn is_empty(&self) -> bool {
        PieceTable::is_empty(self)
    }

    fn insert(&mut self, pos: usize, data: &[T]) {
        PieceTable::insert(self, pos, data)
    }

    fn delete(&mut self, pos: usize, len: usize) {
        PieceTable::delete(self, pos, len)
    }

    fn get(&self, index: usize) -> Option<&T> {
        PieceTable::get(self, index)
    }

    fn validate(&self) -> Result<(), String> {
        PieceTable::validate(self)
    }
}

impl PieceTable<char> {
    fn collect_text(&self) -> String {
        self.iter().copied().collect()
    }

    pub fn grapheme_count(&self) -> usize {
        let text = self.collect_text();
        UnicodeSegmentation::graphemes(text.as_str(), true).count()
    }

    pub fn graphemes(&self) -> Vec<String> {
        let text = self.collect_text();
        UnicodeSegmentation::graphemes(text.as_str(), true)
            .map(ToOwned::to_owned)
            .collect()
    }

    pub fn grapheme_at(&self, index: usize) -> Option<String> {
        let text = self.collect_text();
        UnicodeSegmentation::graphemes(text.as_str(), true)
            .nth(index)
            .map(ToOwned::to_owned)
    }

    pub fn grapheme_range(&self, start: usize, end: usize) -> String {
        assert!(start <= end, "invalid grapheme range: start > end");
        let graphemes = self.graphemes();
        assert!(end <= graphemes.len(), "grapheme range end out of bounds");
        graphemes[start..end].concat()
    }

    pub fn find_grapheme_substring(&self, pattern: &str) -> Vec<usize> {
        if pattern.is_empty() {
            return vec![];
        }

        let text = self.collect_text();
        let text_g: Vec<&str> = UnicodeSegmentation::graphemes(text.as_str(), true).collect();
        let pat_g: Vec<&str> = UnicodeSegmentation::graphemes(pattern, true).collect();

        if pat_g.is_empty() || pat_g.len() > text_g.len() {
            return vec![];
        }

        let mut out = Vec::new();
        for i in 0..=text_g.len() - pat_g.len() {
            if text_g[i..i + pat_g.len()] == pat_g[..] {
                out.push(i);
            }
        }
        out
    }

    pub fn find_regex(&self, pattern: &str) -> Result<Vec<(usize, usize)>, regex::Error> {
        let re = regex::Regex::new(pattern)?;
        let text = self.collect_text();
        Ok(re.find_iter(&text).map(|m| (m.start(), m.end())).collect())
    }

    pub fn find_regex_grapheme(&self, pattern: &str) -> Result<Vec<(usize, usize)>, regex::Error> {
        let re = regex::Regex::new(pattern)?;
        let text = self.collect_text();

        let mut boundaries: Vec<usize> = UnicodeSegmentation::grapheme_indices(text.as_str(), true)
            .map(|(idx, _)| idx)
            .collect();
        boundaries.push(text.len());

        let mut out = Vec::new();
        for m in re.find_iter(&text) {
            let start_g = boundaries
                .partition_point(|&b| b <= m.start())
                .saturating_sub(1);
            let end_g = boundaries.partition_point(|&b| b < m.end());
            out.push((start_g, end_g));
        }

        Ok(out)
    }
}

#[cfg(test)]
mod tests;

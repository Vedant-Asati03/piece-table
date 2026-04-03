use super::{AccessCache, BNode, BufferKind, Piece, PieceTable};

pub struct Cursor<'a, T> {
    table: &'a PieceTable<T>,
    pos: usize,
    piece_start: usize,
    piece_end: usize,
    buffer: Option<BufferKind>,
    buffer_index: usize,
    node_path: Vec<usize>,
    piece_cache: Option<Vec<AccessCache>>,
    piece_cache_idx: usize,
}

impl<'a, T: Clone + PartialEq> Cursor<'a, T> {
    pub(crate) fn new(table: &'a PieceTable<T>, pos: usize) -> Self {
        if table.is_empty() || pos >= table.len() {
            return Self {
                table,
                pos: table.len(),
                piece_start: 0,
                piece_end: 0,
                buffer: None,
                buffer_index: 0,
                node_path: Vec::new(),
                piece_cache: None,
                piece_cache_idx: 0,
            };
        }

        let mut node_path = Vec::new();
        let (piece, offset, piece_start) =
            table
                .root
                .as_ref()
                .unwrap()
                .find_with_path(pos, 0, &mut node_path);
        Self {
            table,
            pos,
            piece_start,
            piece_end: piece_start + piece.len(),
            buffer: Some(piece.buffer),
            buffer_index: piece.start + offset,
            node_path,
            piece_cache: None,
            piece_cache_idx: 0,
        }
    }

    fn ensure_piece_cache(&mut self) {
        if self.piece_cache.is_some() {
            return;
        }

        let mut caches = Vec::new();
        self.table.for_each_piece_in_order(|piece, piece_start| {
            caches.push(AccessCache {
                piece_start,
                piece_end: piece_start + piece.len(),
                buffer: piece.buffer,
                buffer_start: piece.start,
            });
        });

        if caches.is_empty() {
            self.piece_cache = Some(caches);
            self.piece_cache_idx = 0;
            return;
        }

        let mut idx = 0usize;
        while idx + 1 < caches.len() && self.pos >= caches[idx].piece_end {
            idx += 1;
        }

        self.piece_cache = Some(caches);
        self.piece_cache_idx = idx;
    }

    fn load_from_cache_index(&mut self, idx: usize) -> bool {
        let Some(caches) = self.piece_cache.as_ref() else {
            return false;
        };
        if idx >= caches.len() {
            return false;
        }

        let cache = caches[idx];
        if self.pos < cache.piece_start || self.pos >= cache.piece_end {
            return false;
        }

        self.piece_cache_idx = idx;
        self.piece_start = cache.piece_start;
        self.piece_end = cache.piece_end;
        self.buffer = Some(cache.buffer);
        self.buffer_index = cache.buffer_start + (self.pos - cache.piece_start);
        true
    }

    pub fn get(&self) -> Option<&'a T> {
        let buffer = self.buffer?;
        match buffer {
            BufferKind::File => self.table.original.get(self.buffer_index),
            BufferKind::Add => self.table.add.get(self.buffer_index),
        }
    }

    pub fn next(&mut self) -> Option<&'a T> {
        if self.table.is_empty() || self.pos + 1 >= self.table.len() {
            self.buffer = None;
            self.pos = self.table.len();
            return None;
        }

        self.pos += 1;
        if self.pos < self.piece_end {
            self.buffer_index += 1;
            return self.get();
        }

        self.ensure_piece_cache();
        if self.load_from_cache_index(self.piece_cache_idx + 1) {
            return self.get();
        }

        self.node_path.clear();
        let (piece, offset, piece_start) =
            self.table
                .root
                .as_ref()
                .unwrap()
                .find_with_path(self.pos, 0, &mut self.node_path);
        self.piece_start = piece_start;
        self.piece_end = piece_start + piece.len();
        self.buffer = Some(piece.buffer);
        self.buffer_index = piece.start + offset;
        self.get()
    }

    pub fn prev(&mut self) -> Option<&'a T> {
        if self.table.is_empty() || self.pos == 0 || self.pos > self.table.len() {
            return None;
        }

        self.pos -= 1;
        if self.pos >= self.piece_start {
            self.buffer_index = self.buffer_index.saturating_sub(1);
            return self.get();
        }

        self.ensure_piece_cache();
        if self.piece_cache_idx > 0 && self.load_from_cache_index(self.piece_cache_idx - 1) {
            return self.get();
        }

        self.node_path.clear();
        let (piece, offset, piece_start) =
            self.table
                .root
                .as_ref()
                .unwrap()
                .find_with_path(self.pos, 0, &mut self.node_path);
        self.piece_start = piece_start;
        self.piece_end = piece_start + piece.len();
        self.buffer = Some(piece.buffer);
        self.buffer_index = piece.start + offset;
        self.get()
    }
}

pub struct Iter<'a, T> {
    table: &'a PieceTable<T>,
    stack: Vec<(&'a [Box<BNode>], usize)>,
    current_leaf: Option<&'a [Piece]>,
    piece_idx: usize,
    offset_in_piece: usize,
}

impl<'a, T: Clone + PartialEq> Iter<'a, T> {
    pub(crate) fn new(table: &'a PieceTable<T>) -> Self {
        let mut iter = Self {
            table,
            stack: Vec::new(),
            current_leaf: None,
            piece_idx: 0,
            offset_in_piece: 0,
        };

        if let Some(root) = table.root.as_deref() {
            iter.descend_left(root);
        }

        iter
    }

    fn descend_left(&mut self, mut node: &'a BNode) {
        loop {
            match node {
                BNode::Leaf { pieces, .. } => {
                    self.current_leaf = Some(pieces);
                    self.piece_idx = 0;
                    self.offset_in_piece = 0;
                    return;
                }
                BNode::Internal { children, .. } => {
                    if children.is_empty() {
                        self.current_leaf = None;
                        return;
                    }
                    self.stack.push((children, 1));
                    node = children[0].as_ref();
                }
            }
        }
    }

    fn advance_leaf(&mut self) -> bool {
        while let Some((children, next_idx)) = self.stack.last_mut() {
            if *next_idx < children.len() {
                let child = children[*next_idx].as_ref();
                *next_idx += 1;
                self.descend_left(child);
                return true;
            }
            self.stack.pop();
        }

        self.current_leaf = None;
        false
    }
}

impl<'a, T: Clone + PartialEq> Iterator for Iter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let leaf = self.current_leaf?;
            if self.piece_idx >= leaf.len() {
                if !self.advance_leaf() {
                    return None;
                }
                continue;
            }

            let piece = &leaf[self.piece_idx];
            if self.offset_in_piece < piece.len() {
                let idx = piece.start + self.offset_in_piece;
                self.offset_in_piece += 1;
                return match piece.buffer {
                    BufferKind::File => self.table.original.get(idx),
                    BufferKind::Add => self.table.add.get(idx),
                };
            }

            self.piece_idx += 1;
            self.offset_in_piece = 0;
        }
    }
}

pub struct RangeIter<'a, T> {
    pub(crate) cursor: Cursor<'a, T>,
    pub(crate) remaining: usize,
    pub(crate) started: bool,
}

impl<'a, T: Clone + PartialEq> Iterator for RangeIter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }

        let item = if self.started {
            self.cursor.next()?
        } else {
            self.started = true;
            self.cursor.get()?
        };
        self.remaining -= 1;
        Some(item)
    }
}

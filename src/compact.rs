use super::{
    BufferKind, INTERNAL_MAX_CHILDREN, LEAF_MAX_PIECES, Piece, TextBuffer, build_kmp_lps,
    can_coalesce,
};

type CompactNodeId = usize;

#[derive(Debug)]
enum CompactNode {
    Leaf {
        pieces: Vec<Piece>,
        subtree_len: usize,
    },
    Internal {
        children: Vec<CompactNodeId>,
        subtree_len: usize,
    },
}

/// Compact index-arena backend using node IDs instead of pointer-linked ownership.
#[derive(Debug, Default)]
pub struct CompactPieceTable<T> {
    original: Vec<T>,
    add: Vec<T>,
    pieces: Vec<Piece>,
    nodes: Vec<CompactNode>,
    root: Option<CompactNodeId>,
    total_len: usize,
}

impl<T: Clone + PartialEq> CompactPieceTable<T> {
    pub fn new(initial: Vec<T>) -> Self {
        let mut table = Self {
            total_len: initial.len(),
            original: initial,
            add: Vec::new(),
            pieces: Vec::new(),
            nodes: Vec::new(),
            root: None,
        };

        if table.total_len > 0 {
            table.pieces.push(Piece {
                buffer: BufferKind::File,
                start: 0,
                end: table.total_len,
            });
        }

        table.rebuild_index();
        table
    }

    pub fn len(&self) -> usize {
        self.total_len
    }

    pub fn is_empty(&self) -> bool {
        self.total_len == 0
    }

    pub fn height(&self) -> usize {
        let Some(root) = self.root else {
            return 0;
        };

        let mut h = 0usize;
        let mut cur = root;
        loop {
            h += 1;
            match &self.nodes[cur] {
                CompactNode::Leaf { .. } => break,
                CompactNode::Internal { children, .. } => {
                    if children.is_empty() {
                        break;
                    }
                    cur = children[0];
                }
            }
        }
        h
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

        self.insert_piece_at(pos, inserted);
        self.total_len += data.len();
        self.rebuild_index();
    }

    pub fn delete(&mut self, pos: usize, len: usize) {
        assert!(pos <= self.total_len, "delete position out of bounds");
        assert!(
            len <= self.total_len.saturating_sub(pos),
            "delete range out of bounds"
        );
        if len == 0 {
            return;
        }

        let delete_start = pos;
        let delete_end = pos + len;
        let mut out = Vec::with_capacity(self.pieces.len());
        let mut cursor = 0usize;

        for p in &self.pieces {
            let plen = p.len();
            let pstart = cursor;
            let pend = cursor + plen;

            if pend <= delete_start || pstart >= delete_end {
                out.push(p.clone());
            } else {
                if delete_start > pstart {
                    let keep = delete_start - pstart;
                    if keep > 0 {
                        out.push(Piece {
                            buffer: p.buffer,
                            start: p.start,
                            end: p.start + keep,
                        });
                    }
                }

                if delete_end < pend {
                    let keep_from = delete_end - pstart;
                    if keep_from < plen {
                        out.push(Piece {
                            buffer: p.buffer,
                            start: p.start + keep_from,
                            end: p.end,
                        });
                    }
                }
            }

            cursor = pend;
        }

        self.pieces = out;
        self.coalesce_all();
        self.total_len -= len;
        self.rebuild_index();
    }

    pub fn get(&self, index: usize) -> Option<&T> {
        if index >= self.total_len {
            return None;
        }

        let root = self.root?;
        let (piece, offset, _) = self.find_in_index(root, index, 0)?;
        let buf_idx = piece.start + offset;
        match piece.buffer {
            BufferKind::File => self.original.get(buf_idx),
            BufferKind::Add => self.add.get(buf_idx),
        }
    }

    pub fn iter(&self) -> CompactIter<'_, T> {
        CompactIter {
            table: self,
            piece_idx: 0,
            piece_off: 0,
        }
    }

    pub fn range(&self, start: usize, end: usize) -> CompactRangeIter<'_, T> {
        assert!(start <= end, "invalid range: start > end");
        assert!(end <= self.total_len, "range end out of bounds");
        CompactRangeIter {
            iter: self.iter(),
            skip: start,
            remaining: end - start,
        }
    }

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

    pub fn validate(&self) -> Result<(), String> {
        let sum: usize = self.pieces.iter().map(Piece::len).sum();
        if sum != self.total_len {
            return Err(format!(
                "piece sum mismatch: piece_sum={}, total_len={}",
                sum, self.total_len
            ));
        }

        for p in &self.pieces {
            if p.len() == 0 {
                return Err("zero-length piece found".to_string());
            }
            match p.buffer {
                BufferKind::File if p.end > self.original.len() => {
                    return Err("file piece out of bounds".to_string());
                }
                BufferKind::Add if p.end > self.add.len() => {
                    return Err("add piece out of bounds".to_string());
                }
                _ => {}
            }
        }

        for pair in self.pieces.windows(2) {
            if can_coalesce(&pair[0], &pair[1]) {
                return Err("adjacent coalescible pieces found".to_string());
            }
        }

        if let Some(root) = self.root {
            let len = self.node_len(root);
            if len != self.total_len {
                return Err(format!(
                    "index length mismatch: index={}, total={}",
                    len, self.total_len
                ));
            }
        } else if self.total_len != 0 {
            return Err("missing index root on non-empty table".to_string());
        }

        Ok(())
    }

    fn node_len(&self, id: CompactNodeId) -> usize {
        match &self.nodes[id] {
            CompactNode::Leaf { subtree_len, .. } | CompactNode::Internal { subtree_len, .. } => {
                *subtree_len
            }
        }
    }

    fn find_in_index(
        &self,
        node_id: CompactNodeId,
        mut index: usize,
        base: usize,
    ) -> Option<(&Piece, usize, usize)> {
        match &self.nodes[node_id] {
            CompactNode::Leaf { pieces, .. } => {
                let mut pos = base;
                for piece in pieces {
                    let len = piece.len();
                    if index < len {
                        return Some((piece, index, pos));
                    }
                    index -= len;
                    pos += len;
                }
                None
            }
            CompactNode::Internal { children, .. } => {
                let mut consumed = 0usize;
                for child in children {
                    let child_len = self.node_len(*child);
                    if index < child_len {
                        return self.find_in_index(*child, index, base + consumed);
                    }
                    index -= child_len;
                    consumed += child_len;
                }
                None
            }
        }
    }

    fn insert_piece_at(&mut self, pos: usize, inserted: Piece) {
        if self.pieces.is_empty() {
            self.pieces.push(inserted);
            return;
        }

        let mut cursor = 0usize;
        for i in 0..self.pieces.len() {
            let p = &self.pieces[i];
            let len = p.len();
            let start = cursor;
            let end = cursor + len;

            if pos > end {
                cursor = end;
                continue;
            }

            if pos == start {
                self.pieces.insert(i, inserted);
                self.coalesce_around(i);
                return;
            }

            if pos == end {
                self.pieces.insert(i + 1, inserted);
                self.coalesce_around(i + 1);
                return;
            }

            let split_at = pos - start;
            let left_piece = Piece {
                buffer: p.buffer,
                start: p.start,
                end: p.start + split_at,
            };
            let right_piece = Piece {
                buffer: p.buffer,
                start: p.start + split_at,
                end: p.end,
            };

            self.pieces[i] = left_piece;
            self.pieces.insert(i + 1, inserted);
            self.pieces.insert(i + 2, right_piece);
            self.coalesce_around(i + 1);
            return;
        }

        self.pieces.push(inserted);
        self.coalesce_around(self.pieces.len() - 1);
    }

    fn coalesce_around(&mut self, mut idx: usize) {
        while idx > 0 {
            if can_coalesce(&self.pieces[idx - 1], &self.pieces[idx]) {
                let end = self.pieces[idx].end;
                self.pieces[idx - 1].end = end;
                self.pieces.remove(idx);
                idx -= 1;
            } else {
                break;
            }
        }

        while idx + 1 < self.pieces.len() {
            if can_coalesce(&self.pieces[idx], &self.pieces[idx + 1]) {
                let end = self.pieces[idx + 1].end;
                self.pieces[idx].end = end;
                self.pieces.remove(idx + 1);
            } else {
                break;
            }
        }
    }

    fn coalesce_all(&mut self) {
        if self.pieces.len() < 2 {
            return;
        }

        let mut out = Vec::with_capacity(self.pieces.len());
        out.push(self.pieces[0].clone());
        for p in self.pieces.iter().skip(1) {
            if let Some(last) = out.last_mut()
                && can_coalesce(last, p)
            {
                last.end = p.end;
            } else {
                out.push(p.clone());
            }
        }
        self.pieces = out;
    }

    fn rebuild_index(&mut self) {
        self.nodes.clear();
        self.root = None;

        if self.pieces.is_empty() {
            return;
        }

        let mut level = Vec::new();
        for chunk in self.pieces.chunks(LEAF_MAX_PIECES) {
            let pieces = chunk.to_vec();
            let subtree_len = pieces.iter().map(Piece::len).sum();
            let id = self.nodes.len();
            self.nodes.push(CompactNode::Leaf {
                pieces,
                subtree_len,
            });
            level.push(id);
        }

        while level.len() > 1 {
            let mut next = Vec::new();
            for group in level.chunks(INTERNAL_MAX_CHILDREN) {
                let children = group.to_vec();
                let subtree_len = children.iter().map(|id| self.node_len(*id)).sum();
                let id = self.nodes.len();
                self.nodes.push(CompactNode::Internal {
                    children,
                    subtree_len,
                });
                next.push(id);
            }
            level = next;
        }

        self.root = level.first().copied();
    }
}

impl<T: Clone + PartialEq> TextBuffer<T> for CompactPieceTable<T> {
    fn len(&self) -> usize {
        CompactPieceTable::len(self)
    }

    fn is_empty(&self) -> bool {
        CompactPieceTable::is_empty(self)
    }

    fn insert(&mut self, pos: usize, data: &[T]) {
        CompactPieceTable::insert(self, pos, data)
    }

    fn delete(&mut self, pos: usize, len: usize) {
        CompactPieceTable::delete(self, pos, len)
    }

    fn get(&self, index: usize) -> Option<&T> {
        CompactPieceTable::get(self, index)
    }

    fn validate(&self) -> Result<(), String> {
        CompactPieceTable::validate(self)
    }
}

pub struct CompactIter<'a, T> {
    table: &'a CompactPieceTable<T>,
    piece_idx: usize,
    piece_off: usize,
}

impl<'a, T: Clone + PartialEq> Iterator for CompactIter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let piece = self.table.pieces.get(self.piece_idx)?;
            if self.piece_off < piece.len() {
                let idx = piece.start + self.piece_off;
                self.piece_off += 1;
                return match piece.buffer {
                    BufferKind::File => self.table.original.get(idx),
                    BufferKind::Add => self.table.add.get(idx),
                };
            }

            self.piece_idx += 1;
            self.piece_off = 0;
        }
    }
}

pub struct CompactRangeIter<'a, T> {
    iter: CompactIter<'a, T>,
    skip: usize,
    remaining: usize,
}

impl<'a, T: Clone + PartialEq> Iterator for CompactRangeIter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        while self.skip > 0 {
            self.iter.next()?;
            self.skip -= 1;
        }

        if self.remaining == 0 {
            return None;
        }

        let item = self.iter.next()?;
        self.remaining -= 1;
        Some(item)
    }
}

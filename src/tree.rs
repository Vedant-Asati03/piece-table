use super::{
    BufferKind, INTERNAL_MAX_CHILDREN, INTERNAL_MIN_CHILDREN, LEAF_MAX_PIECES, LEAF_MIN_PIECES,
    Piece, Stats,
};

#[derive(Debug)]
pub(crate) enum BNode {
    Leaf {
        pieces: Vec<Piece>,
        subtree_len: usize,
    },
    Internal {
        children: Vec<Box<BNode>>,
        subtree_len: usize,
    },
}

pub(crate) type Tree = Option<Box<BNode>>;

#[derive(Debug, Default)]
pub(crate) struct NodePool {
    free: Vec<Box<BNode>>,
}

impl NodePool {
    pub(crate) fn alloc(&mut self, node: BNode) -> Box<BNode> {
        if let Some(mut slot) = self.free.pop() {
            *slot = node;
            slot
        } else {
            Box::new(node)
        }
    }

    pub(crate) fn recycle(&mut self, mut node: Box<BNode>) {
        match node.as_mut() {
            BNode::Leaf { pieces, .. } => {
                pieces.clear();
            }
            BNode::Internal { children, .. } => {
                let drained = std::mem::take(children);
                for child in drained {
                    self.recycle(child);
                }
            }
        }
        self.free.push(node);
    }
}

impl BNode {
    pub(crate) fn len(&self) -> usize {
        match self {
            BNode::Leaf { subtree_len, .. } | BNode::Internal { subtree_len, .. } => *subtree_len,
        }
    }

    pub(crate) fn height(&self) -> usize {
        match self {
            BNode::Leaf { .. } => 1,
            BNode::Internal { children, .. } => {
                1 + children.first().map_or(0, |child| child.height())
            }
        }
    }

    pub(crate) fn update_len(&mut self) {
        match self {
            BNode::Leaf {
                pieces,
                subtree_len,
            } => {
                *subtree_len = pieces.iter().map(Piece::len).sum();
            }
            BNode::Internal {
                children,
                subtree_len,
            } => {
                *subtree_len = children.iter().map(|child| child.len()).sum();
            }
        }
    }

    pub(crate) fn compact_root(mut self: Box<Self>, pool: &mut NodePool) -> Box<Self> {
        loop {
            match *self {
                BNode::Internal {
                    ref mut children, ..
                } if children.len() == 1 => {
                    let only = children.remove(0);
                    pool.recycle(self);
                    self = only;
                }
                _ => {
                    self.update_len();
                    return self;
                }
            }
        }
    }

    pub(crate) fn find(&self, mut index: usize, base: usize) -> (&Piece, usize, usize) {
        match self {
            BNode::Leaf { pieces, .. } => {
                let mut pos = base;
                for piece in pieces {
                    let len = piece.len();
                    if index < len {
                        return (piece, index, pos);
                    }
                    index -= len;
                    pos += len;
                }
                unreachable!("index out of bounds in leaf find")
            }
            BNode::Internal { children, .. } => {
                let mut consumed = 0usize;
                for child in children {
                    let child_len = child.len();
                    if index < child_len {
                        return child.find(index, base + consumed);
                    }
                    index -= child_len;
                    consumed += child_len;
                }
                unreachable!("index out of bounds in internal find")
            }
        }
    }

    pub(crate) fn find_with_path<'a>(
        &'a self,
        mut index: usize,
        base: usize,
        path: &mut Vec<usize>,
    ) -> (&'a Piece, usize, usize) {
        match self {
            BNode::Leaf { pieces, .. } => {
                let mut pos = base;
                for piece in pieces {
                    let len = piece.len();
                    if index < len {
                        return (piece, index, pos);
                    }
                    index -= len;
                    pos += len;
                }
                unreachable!("index out of bounds in leaf find_with_path")
            }
            BNode::Internal { children, .. } => {
                let (child_idx, child_start) = child_index_and_start_for_pos(children, index);
                path.push(child_idx);
                children[child_idx].find_with_path(index - child_start, base + child_start, path)
            }
        }
    }

    pub(crate) fn insert_at(
        &mut self,
        pos: usize,
        piece: Piece,
        stats: &mut Stats,
        pool: &mut NodePool,
    ) -> Option<Box<BNode>> {
        match self {
            BNode::Leaf {
                pieces,
                subtree_len,
            } => {
                leaf_insert_piece(pieces, pos, piece, stats);
                *subtree_len = pieces.iter().map(Piece::len).sum();
                if pieces.len() > LEAF_MAX_PIECES {
                    stats.split_calls += 1;
                    let mid = pieces.len() / 2;
                    let right_pieces = pieces.split_off(mid);
                    let right_len: usize = right_pieces.iter().map(Piece::len).sum();
                    *subtree_len = pieces.iter().map(Piece::len).sum();
                    Some(pool.alloc(BNode::Leaf {
                        pieces: right_pieces,
                        subtree_len: right_len,
                    }))
                } else {
                    None
                }
            }
            BNode::Internal {
                children,
                subtree_len,
            } => {
                let (child_idx, child_start) = child_index_and_start_for_pos(children, pos);
                let local_pos = pos - child_start;

                let split = children[child_idx].insert_at(local_pos, piece, stats, pool);
                if let Some(right_child) = split {
                    if children.len() + 1 >= children.capacity()
                        && children.capacity() < INTERNAL_MAX_CHILDREN
                    {
                        children.reserve(INTERNAL_MAX_CHILDREN - children.capacity());
                    }
                    children.insert(child_idx + 1, right_child);
                }

                normalize_adjacent_leaves(children, stats);
                split_overfull_children(children, stats, pool);
                *subtree_len = children.iter().map(|c| c.len()).sum();

                if children.len() > INTERNAL_MAX_CHILDREN {
                    stats.split_calls += 1;
                    let mid = children.len() / 2;
                    let right_children = children.split_off(mid);
                    let right_len: usize = right_children.iter().map(|c| c.len()).sum();
                    *subtree_len = children.iter().map(|c| c.len()).sum();
                    Some(pool.alloc(BNode::Internal {
                        children: right_children,
                        subtree_len: right_len,
                    }))
                } else {
                    None
                }
            }
        }
    }

    pub(crate) fn try_extend_add_at_pos(
        &mut self,
        pos: usize,
        append_start: usize,
        append_end: usize,
        stats: &mut Stats,
        pool: &mut NodePool,
    ) -> bool {
        match self {
            BNode::Leaf {
                pieces,
                subtree_len,
            } => {
                let mut cursor = 0usize;
                for i in 0..pieces.len() {
                    let len = pieces[i].len();
                    let piece_end_pos = cursor + len;
                    if pos <= piece_end_pos {
                        if pos == piece_end_pos
                            && pieces[i].buffer == BufferKind::Add
                            && pieces[i].end == append_start
                        {
                            pieces[i].end = append_end;
                            if i + 1 < pieces.len() && can_coalesce(&pieces[i], &pieces[i + 1]) {
                                let end = pieces[i + 1].end;
                                pieces[i].end = end;
                                pieces.remove(i + 1);
                                stats.coalesces += 1;
                                stats.merge_calls += 1;
                            }
                            *subtree_len = pieces.iter().map(Piece::len).sum();
                            return true;
                        }
                        return false;
                    }
                    cursor = piece_end_pos;
                }
                false
            }
            BNode::Internal {
                children,
                subtree_len,
            } => {
                if children.is_empty() {
                    return false;
                }

                let idx = child_index_for_pos(children, pos.saturating_sub(1));
                let did =
                    children[idx].try_extend_add_at_pos(pos, append_start, append_end, stats, pool);
                if did {
                    normalize_adjacent_leaves(children.as_mut_slice(), stats);
                    split_overfull_children(children, stats, pool);
                    *subtree_len = children.iter().map(|c| c.len()).sum();
                }
                did
            }
        }
    }

    pub(crate) fn delete_range(
        &mut self,
        pos: usize,
        len: usize,
        stats: &mut Stats,
        pool: &mut NodePool,
    ) {
        if len == 0 || self.len() == 0 {
            return;
        }

        match self {
            BNode::Leaf {
                pieces,
                subtree_len,
            } => {
                leaf_delete_range(pieces, pos, len, stats);
                *subtree_len = pieces.iter().map(Piece::len).sum();
            }
            BNode::Internal {
                children,
                subtree_len,
            } => {
                let mut remaining = len;
                let global_pos = pos;

                while remaining > 0 {
                    if children.is_empty() {
                        break;
                    }

                    let current_len: usize = children.iter().map(|c| c.len()).sum();
                    let (idx, child_start) = child_index_and_start_for_pos(
                        children,
                        global_pos.min(current_len.saturating_sub(1)),
                    );
                    let local_pos = global_pos.saturating_sub(child_start);
                    let child_available = children[idx].len().saturating_sub(local_pos);
                    let chunk = remaining.min(child_available.max(1));

                    children[idx].delete_range(local_pos, chunk, stats, pool);
                    if children[idx].len() == 0 {
                        let removed = children.remove(idx);
                        pool.recycle(removed);
                    }

                    remaining -= chunk;
                }

                rebalance_children(children, stats, pool);
                normalize_adjacent_leaves(children.as_mut_slice(), stats);
                split_overfull_children(children, stats, pool);
                *subtree_len = children.iter().map(|c| c.len()).sum();
            }
        }
    }

    pub(crate) fn validate_and_collect(&self, out: &mut Vec<Piece>) -> Result<usize, String> {
        match self {
            BNode::Leaf {
                pieces,
                subtree_len,
            } => {
                let computed: usize = pieces.iter().map(Piece::len).sum();
                if computed != *subtree_len {
                    return Err(format!(
                        "leaf subtree_len mismatch: computed={}, stored={}",
                        computed, subtree_len
                    ));
                }

                out.extend(pieces.iter().cloned());
                Ok(computed)
            }
            BNode::Internal {
                children,
                subtree_len,
            } => {
                if children.is_empty() {
                    return Err("internal node has no children".to_string());
                }

                let mut computed = 0usize;
                for child in children {
                    computed += child.validate_and_collect(out)?;
                }

                if computed != *subtree_len {
                    return Err(format!(
                        "internal subtree_len mismatch: computed={}, stored={}",
                        computed, subtree_len
                    ));
                }

                Ok(computed)
            }
        }
    }
}

fn child_index_for_pos(children: &[Box<BNode>], pos: usize) -> usize {
    child_index_and_start_for_pos(children, pos).0
}

fn child_index_and_start_for_pos(children: &[Box<BNode>], pos: usize) -> (usize, usize) {
    if children.is_empty() {
        return (0, 0);
    }

    let mut consumed = 0usize;
    for (i, child) in children.iter().enumerate() {
        let end = consumed + child.len();
        if pos < end {
            return (i, consumed);
        }
        consumed = end;
    }

    let last_idx = children.len() - 1;
    let last_start = consumed.saturating_sub(children[last_idx].len());
    (last_idx, last_start)
}

pub(crate) fn can_coalesce(left: &Piece, right: &Piece) -> bool {
    left.buffer == right.buffer && left.end == right.start
}

fn leaf_insert_piece(pieces: &mut Vec<Piece>, pos: usize, inserted: Piece, stats: &mut Stats) {
    if pieces.is_empty() {
        if pieces.capacity() < LEAF_MAX_PIECES {
            pieces.reserve(LEAF_MAX_PIECES - pieces.capacity());
        }
        pieces.push(inserted);
        return;
    }

    if pieces.len() + 3 >= pieces.capacity() && pieces.capacity() < LEAF_MAX_PIECES {
        pieces.reserve(LEAF_MAX_PIECES - pieces.capacity());
    }

    let mut cursor = 0usize;
    for i in 0..pieces.len() {
        let p = &pieces[i];
        let len = p.len();
        let start = cursor;
        let end = cursor + len;

        if pos > end {
            cursor = end;
            continue;
        }

        if pos == start {
            pieces.insert(i, inserted);
            coalesce_around(pieces, i, stats);
            return;
        }

        if pos == end {
            pieces.insert(i + 1, inserted);
            coalesce_around(pieces, i + 1, stats);
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

        pieces[i] = left_piece;
        pieces.insert(i + 1, inserted);
        pieces.insert(i + 2, right_piece);
        stats.split_calls += 1;
        coalesce_around(pieces, i + 1, stats);
        return;
    }

    pieces.push(inserted);
    let idx = pieces.len() - 1;
    coalesce_around(pieces, idx, stats);
}

fn leaf_delete_range(pieces: &mut Vec<Piece>, pos: usize, len: usize, stats: &mut Stats) {
    if len == 0 || pieces.is_empty() {
        return;
    }

    let delete_start = pos;
    let delete_end = pos + len;
    let mut out = Vec::with_capacity(pieces.len());
    let mut cursor = 0usize;

    for p in pieces.iter() {
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
                    stats.split_calls += 1;
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
                    stats.split_calls += 1;
                }
            }
        }

        cursor = pend;
    }

    *pieces = out;

    if pieces.len() > 1 {
        let mut i = 1usize;
        while i < pieces.len() {
            if can_coalesce(&pieces[i - 1], &pieces[i]) {
                let end = pieces[i].end;
                pieces[i - 1].end = end;
                pieces.remove(i);
                stats.coalesces += 1;
                stats.merge_calls += 1;
            } else {
                i += 1;
            }
        }
    }
}

fn coalesce_around(pieces: &mut Vec<Piece>, mut idx: usize, stats: &mut Stats) {
    while idx > 0 {
        if can_coalesce(&pieces[idx - 1], &pieces[idx]) {
            let end = pieces[idx].end;
            pieces[idx - 1].end = end;
            pieces.remove(idx);
            idx -= 1;
            stats.coalesces += 1;
            stats.merge_calls += 1;
        } else {
            break;
        }
    }

    while idx + 1 < pieces.len() {
        if can_coalesce(&pieces[idx], &pieces[idx + 1]) {
            let end = pieces[idx + 1].end;
            pieces[idx].end = end;
            pieces.remove(idx + 1);
            stats.coalesces += 1;
            stats.merge_calls += 1;
        } else {
            break;
        }
    }
}

fn normalize_adjacent_leaves(children: &mut [Box<BNode>], stats: &mut Stats) {
    if children.len() < 2 {
        return;
    }

    for i in 0..children.len() - 1 {
        let (left, right) = children.split_at_mut(i + 1);
        let lnode = &mut left[i];
        let rnode = &mut right[0];

        let BNode::Leaf { pieces: lp, .. } = lnode.as_mut() else {
            continue;
        };
        let BNode::Leaf { pieces: rp, .. } = rnode.as_mut() else {
            continue;
        };

        if lp.is_empty() || rp.is_empty() {
            continue;
        }

        if can_coalesce(lp.last().unwrap(), rp.first().unwrap()) {
            let right_first = rp.remove(0);
            lp.last_mut().unwrap().end = right_first.end;
            stats.coalesces += 1;
            stats.merge_calls += 1;
            lnode.update_len();
            rnode.update_len();
        }
    }
}

fn split_overfull_children(children: &mut Vec<Box<BNode>>, stats: &mut Stats, pool: &mut NodePool) {
    let mut i = 0usize;
    while i < children.len() {
        let right_split = match children[i].as_mut() {
            BNode::Leaf {
                pieces,
                subtree_len,
            } if pieces.len() > LEAF_MAX_PIECES => {
                let mid = pieces.len() / 2;
                let right_pieces = pieces.split_off(mid);
                let right_len: usize = right_pieces.iter().map(Piece::len).sum();
                *subtree_len = pieces.iter().map(Piece::len).sum();
                Some(pool.alloc(BNode::Leaf {
                    pieces: right_pieces,
                    subtree_len: right_len,
                }))
            }
            BNode::Internal {
                children: inner,
                subtree_len,
            } if inner.len() > INTERNAL_MAX_CHILDREN => {
                let mid = inner.len() / 2;
                let right_children = inner.split_off(mid);
                let right_len: usize = right_children.iter().map(|c| c.len()).sum();
                *subtree_len = inner.iter().map(|c| c.len()).sum();
                Some(pool.alloc(BNode::Internal {
                    children: right_children,
                    subtree_len: right_len,
                }))
            }
            _ => None,
        };

        if let Some(right) = right_split {
            children.insert(i + 1, right);
            stats.split_calls += 1;
            i += 2;
        } else {
            i += 1;
        }
    }
}

fn is_underfull(node: &BNode) -> bool {
    match node {
        BNode::Leaf { pieces, .. } => !pieces.is_empty() && pieces.len() < LEAF_MIN_PIECES,
        BNode::Internal { children, .. } => {
            !children.is_empty() && children.len() < INTERNAL_MIN_CHILDREN
        }
    }
}

fn can_lend(node: &BNode) -> bool {
    match node {
        BNode::Leaf { pieces, .. } => pieces.len() > LEAF_MIN_PIECES,
        BNode::Internal { children, .. } => children.len() > INTERNAL_MIN_CHILDREN,
    }
}

fn same_kind(left: &BNode, right: &BNode) -> bool {
    matches!(
        (left, right),
        (BNode::Leaf { .. }, BNode::Leaf { .. }) | (BNode::Internal { .. }, BNode::Internal { .. })
    )
}

fn borrow_from_left(children: &mut [Box<BNode>], idx: usize) -> bool {
    if idx == 0 {
        return false;
    }

    let (left, right) = children.split_at_mut(idx);
    let left_node = &mut left[idx - 1];
    let cur_node = &mut right[0];

    match (left_node.as_mut(), cur_node.as_mut()) {
        (
            BNode::Leaf {
                pieces: left_pieces,
                ..
            },
            BNode::Leaf {
                pieces: cur_pieces, ..
            },
        ) if left_pieces.len() > LEAF_MIN_PIECES => {
            let moved = left_pieces.pop().unwrap();
            cur_pieces.insert(0, moved);
            left_node.update_len();
            cur_node.update_len();
            true
        }
        (
            BNode::Internal {
                children: left_children,
                ..
            },
            BNode::Internal {
                children: cur_children,
                ..
            },
        ) if left_children.len() > INTERNAL_MIN_CHILDREN => {
            let moved = left_children.pop().unwrap();
            cur_children.insert(0, moved);
            left_node.update_len();
            cur_node.update_len();
            true
        }
        _ => false,
    }
}

fn borrow_from_right(children: &mut [Box<BNode>], idx: usize) -> bool {
    if idx + 1 >= children.len() {
        return false;
    }

    let (left, right) = children.split_at_mut(idx + 1);
    let cur_node = &mut left[idx];
    let right_node = &mut right[0];

    match (cur_node.as_mut(), right_node.as_mut()) {
        (
            BNode::Leaf {
                pieces: cur_pieces, ..
            },
            BNode::Leaf {
                pieces: right_pieces,
                ..
            },
        ) if right_pieces.len() > LEAF_MIN_PIECES => {
            let moved = right_pieces.remove(0);
            cur_pieces.push(moved);
            cur_node.update_len();
            right_node.update_len();
            true
        }
        (
            BNode::Internal {
                children: cur_children,
                ..
            },
            BNode::Internal {
                children: right_children,
                ..
            },
        ) if right_children.len() > INTERNAL_MIN_CHILDREN => {
            let moved = right_children.remove(0);
            cur_children.push(moved);
            cur_node.update_len();
            right_node.update_len();
            true
        }
        _ => false,
    }
}

fn merge_children(
    children: &mut Vec<Box<BNode>>,
    left_idx: usize,
    right_idx: usize,
    stats: &mut Stats,
    pool: &mut NodePool,
) {
    if right_idx >= children.len() || left_idx >= children.len() || left_idx >= right_idx {
        return;
    }

    let mut right = children.remove(right_idx);
    let left = &mut children[left_idx];

    match (left.as_mut(), right.as_mut()) {
        (
            BNode::Leaf {
                pieces: left_pieces,
                ..
            },
            BNode::Leaf {
                pieces: right_pieces,
                ..
            },
        ) => {
            left_pieces.append(right_pieces);
            left.update_len();
            stats.merge_calls += 1;
            pool.recycle(right);
        }
        (
            BNode::Internal {
                children: left_children,
                ..
            },
            BNode::Internal {
                children: right_children,
                ..
            },
        ) => {
            left_children.append(right_children);
            left.update_len();
            stats.merge_calls += 1;
            pool.recycle(right);
        }
        _ => {}
    }
}

fn rebalance_children(children: &mut Vec<Box<BNode>>, stats: &mut Stats, pool: &mut NodePool) {
    if children.len() < 2 {
        return;
    }

    let mut i = 0usize;
    while i < children.len() {
        if !is_underfull(children[i].as_ref()) {
            i += 1;
            continue;
        }

        let borrowed_left = i > 0
            && same_kind(children[i - 1].as_ref(), children[i].as_ref())
            && can_lend(children[i - 1].as_ref())
            && borrow_from_left(children.as_mut_slice(), i);

        if borrowed_left {
            i = i.saturating_sub(1);
            continue;
        }

        let borrowed_right = i + 1 < children.len()
            && same_kind(children[i].as_ref(), children[i + 1].as_ref())
            && can_lend(children[i + 1].as_ref())
            && borrow_from_right(children.as_mut_slice(), i);

        if borrowed_right {
            i = i.saturating_sub(1);
            continue;
        }

        if i > 0 && same_kind(children[i - 1].as_ref(), children[i].as_ref()) {
            merge_children(children, i - 1, i, stats, pool);
            i = i.saturating_sub(1);
            continue;
        }

        if i + 1 < children.len() && same_kind(children[i].as_ref(), children[i + 1].as_ref()) {
            merge_children(children, i, i + 1, stats, pool);
            continue;
        }

        i += 1;
    }
}

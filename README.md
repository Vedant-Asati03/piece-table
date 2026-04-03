# pbtree

`pbtree` is a generic piece-table text buffer for editor workloads.

It is designed for efficient edits (insert/delete in the middle of large documents), while still supporting fast iteration, range access, and substring search.

## Features

- Generic buffer core: `PieceTable<T>`
- Balanced B+ tree storage for scalable edit operations
- Cursor and range iteration APIs
- Search APIs:
	- KMP substring search (generic)
	- optimized byte search paths (`u8`)
	- regex search (`u8` and `char` variants)
	- grapheme-aware helpers for `char`
- Optional compact backend: `CompactPieceTable<T>`

## Quick start

```rust
use pbtree::PieceTable;

let mut table = PieceTable::new("hello".chars().collect());
table.insert(5, &[' ', 'w', 'o', 'r', 'l', 'd']);
table.delete(0, 1);

let text: String = table.iter().copied().collect();
assert_eq!(text, "ello world");
```

## Compact backend

```rust
use pbtree::{CompactPieceTable, TextBuffer};

let mut table = CompactPieceTable::new(Vec::<char>::new());
table.insert(0, &['a', 'b', 'c']);
assert_eq!(table.get(1), Some(&'b'));
assert!(table.validate().is_ok());
```

## Status

This crate is actively used in the `rusk` editor project and benchmarked on large-document workloads.

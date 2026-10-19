// SPDX-License-Identifier: AGPL-3.0-or-later

use std::mem::size_of;

const BLOCK_HEADER: usize = size_of::<usize>();
const BLOCK_ALIGN: usize = 16;
pub(crate) const MIN_BLOCK: usize = 32;
const BTREE_CAPACITY: usize = 11;
const BTREE_MIN_LEN: usize = 5;

#[must_use]
pub(crate) fn block(bytes: usize) -> usize {
    if bytes == 0 {
        return 0;
    }
    bytes
        .saturating_add(BLOCK_HEADER)
        .checked_next_multiple_of(BLOCK_ALIGN)
        .unwrap_or(usize::MAX)
        .max(MIN_BLOCK)
}

fn min_capacity<T>() -> usize {
    match size_of::<T>() {
        1 => 8,
        size if size <= 1024 => 4,
        _ => 1,
    }
}

#[must_use]
fn vec_capacity<T>(len: usize) -> usize {
    if len == 0 {
        0
    } else {
        len.saturating_mul(2).max(min_capacity::<T>())
    }
}

#[must_use]
pub(crate) fn vec_bytes<T>(len: usize) -> usize {
    block(size_of::<T>().saturating_mul(vec_capacity::<T>(len)))
}

#[must_use]
pub(crate) fn vecs_bytes<T>(vecs: usize, len: usize) -> usize {
    if size_of::<T>() == 0 || len == 0 {
        return 0;
    }
    let vecs = vecs.min(len);
    vecs.saturating_mul(
        size_of::<T>()
            .saturating_mul(min_capacity::<T>())
            .saturating_add(MIN_BLOCK),
    )
    .saturating_add(size_of::<T>().saturating_mul(len).saturating_mul(2))
}

#[must_use]
pub(crate) fn btree_bytes<K, V>(maps: usize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let leaf = block(size_of::<(
        usize,
        u16,
        u16,
        [K; BTREE_CAPACITY],
        [V; BTREE_CAPACITY],
    )>());
    let internal = block(size_of::<(
        (usize, u16, u16, [K; BTREE_CAPACITY], [V; BTREE_CAPACITY]),
        [usize; BTREE_CAPACITY + 1],
    )>());
    let nodes = len.min(maps.saturating_add(len / BTREE_MIN_LEN));
    nodes
        .saturating_mul(leaf)
        .saturating_add((len / BTREE_CAPACITY).saturating_mul(internal.saturating_sub(leaf)))
}

//! Collection types, including `HashMap`.

pub use alloc::collections::*;

use alloc::vec::Vec;
use core::borrow::Borrow;
use core::hash::{Hash, Hasher};

struct FnvHasher(u64);

impl FnvHasher {
    const OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;

    #[inline]
    fn new() -> Self {
        Self(Self::OFFSET_BASIS)
    }
}

impl Hasher for FnvHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }
}

#[inline]
fn hash_key<Q: Hash + ?Sized>(key: &Q) -> u64 {
    let mut hasher = FnvHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

enum Bucket<K, V> {
    Empty,
    Tombstone,
    Occupied(K, V),
}

/// A hash map implemented with open addressing and linear probing.
pub struct HashMap<K, V> {
    buckets: Vec<Bucket<K, V>>,
    len: usize,
    tombstones: usize,
}

impl<K, V> Default for HashMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> HashMap<K, V> {
    /// Creates an empty `HashMap`.
    pub const fn new() -> Self {
        Self {
            buckets: Vec::new(),
            len: 0,
            tombstones: 0,
        }
    }

    /// Creates an empty `HashMap` with at least the specified capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        if capacity == 0 {
            return Self::new();
        }
        let cap = (capacity.saturating_mul(10) / 7 + 1)
            .max(16)
            .next_power_of_two();
        let mut buckets = Vec::with_capacity(cap);
        for _ in 0..cap {
            buckets.push(Bucket::Empty);
        }
        Self {
            buckets,
            len: 0,
            tombstones: 0,
        }
    }

    /// Returns the number of elements in the map.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if the map contains no elements.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// An iterator visiting all key-value pairs in arbitrary order.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.buckets.iter().filter_map(|b| match b {
            Bucket::Occupied(k, v) => Some((k, v)),
            _ => None,
        })
    }
}

impl<K: Eq + Hash, V> HashMap<K, V> {
    fn resize(&mut self, new_cap: usize) {
        let mut new_buckets = Vec::with_capacity(new_cap);
        for _ in 0..new_cap {
            new_buckets.push(Bucket::Empty);
        }
        let mask = new_cap - 1;
        for bucket in self.buckets.drain(..) {
            if let Bucket::Occupied(k, v) = bucket {
                let mut idx = (hash_key(&k) as usize) & mask;
                loop {
                    if matches!(new_buckets[idx], Bucket::Empty) {
                        new_buckets[idx] = Bucket::Occupied(k, v);
                        break;
                    }
                    idx = (idx + 1) & mask;
                }
            }
        }
        self.buckets = new_buckets;
        self.tombstones = 0;
    }

    fn ensure_capacity(&mut self) {
        let cap = self.buckets.len();
        if cap == 0 {
            self.resize(16);
        } else if (self.len + self.tombstones + 1) * 10 > cap * 7 {
            let new_cap = if self.len * 10 > cap * 5 {
                cap * 2
            } else {
                cap
            };
            self.resize(new_cap);
        }
    }

    /// Inserts a key-value pair into the map.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.ensure_capacity();
        let mask = self.buckets.len() - 1;
        let mut idx = (hash_key(&key) as usize) & mask;
        let mut first_tombstone: Option<usize> = None;

        loop {
            match &mut self.buckets[idx] {
                Bucket::Empty => {
                    let insert_idx = first_tombstone.unwrap_or(idx);
                    if first_tombstone.is_some() {
                        self.tombstones -= 1;
                    }
                    self.buckets[insert_idx] = Bucket::Occupied(key, value);
                    self.len += 1;
                    return None;
                }
                Bucket::Tombstone => {
                    if first_tombstone.is_none() {
                        first_tombstone = Some(idx);
                    }
                }
                Bucket::Occupied(k, v) => {
                    if *k == key {
                        return Some(core::mem::replace(v, value));
                    }
                }
            }
            idx = (idx + 1) & mask;
        }
    }

    /// Returns a reference to the value corresponding to the key.
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        if self.buckets.is_empty() {
            return None;
        }
        let mask = self.buckets.len() - 1;
        let mut idx = (hash_key(key) as usize) & mask;
        loop {
            match &self.buckets[idx] {
                Bucket::Empty => return None,
                Bucket::Tombstone => {}
                Bucket::Occupied(k, v) => {
                    if k.borrow() == key {
                        return Some(v);
                    }
                }
            }
            idx = (idx + 1) & mask;
        }
    }

    /// Returns a mutable reference to the value corresponding to the key.
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        if self.buckets.is_empty() {
            return None;
        }
        let mask = self.buckets.len() - 1;
        let mut idx = (hash_key(key) as usize) & mask;
        let found = loop {
            match &self.buckets[idx] {
                Bucket::Empty => return None,
                Bucket::Tombstone => {}
                Bucket::Occupied(k, _) => {
                    if k.borrow() == key {
                        break idx;
                    }
                }
            }
            idx = (idx + 1) & mask;
        };
        if let Bucket::Occupied(_, v) = &mut self.buckets[found] {
            Some(v)
        } else {
            None
        }
    }

    /// Returns `true` if the map contains a value for the specified key.
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.get(key).is_some()
    }

    /// Removes a key from the map, returning the value at the key if it was previously in the map.
    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        if self.buckets.is_empty() {
            return None;
        }
        let mask = self.buckets.len() - 1;
        let mut idx = (hash_key(key) as usize) & mask;
        loop {
            match &self.buckets[idx] {
                Bucket::Empty => return None,
                Bucket::Tombstone => {}
                Bucket::Occupied(k, _) => {
                    if k.borrow() == key {
                        let old = core::mem::replace(&mut self.buckets[idx], Bucket::Tombstone);
                        self.len -= 1;
                        self.tombstones += 1;
                        if let Bucket::Occupied(_, v) = old {
                            return Some(v);
                        }
                    }
                }
            }
            idx = (idx + 1) & mask;
        }
    }
}

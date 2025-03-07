//! Archived hash map implementation.
//!
//! During archiving, hashmaps are built into minimal perfect hashmaps using
//! [compress, hash and displace](http://cmph.sourceforge.net/papers/esa09.pdf).

#[cfg(feature = "validation")]
pub mod validation;

use crate::{
    collections::{
        hash_index::{ArchivedHashIndex, HashIndexResolver},
        util::Entry,
    },
    RelPtr,
};
#[cfg(feature = "alloc")]
use crate::{
    ser::{ScratchSpace, Serializer},
    Serialize,
};
use core::{
    borrow::Borrow, fmt, hash::Hash, iter::FusedIterator, marker::PhantomData, ops::Index, pin::Pin,
};

/// An archived `HashMap`.
#[cfg_attr(feature = "strict", repr(C))]
pub struct ArchivedHashMap<K, V> {
    index: ArchivedHashIndex,
    entries: RelPtr<Entry<K, V>>,
}

impl<K, V> ArchivedHashMap<K, V> {
    /// Gets the number of items in the hash map.
    #[inline]
    pub const fn len(&self) -> usize {
        self.index.len()
    }

    /// Gets the hasher for this hashmap. The hasher for all archived hashmaps is the same for
    /// reproducibility.
    #[inline]
    pub fn hasher(&self) -> seahash::SeaHasher {
        self.index.hasher()
    }

    #[inline]
    unsafe fn entry(&self, index: usize) -> &Entry<K, V> {
        &*self.entries.as_ptr().add(index)
    }

    #[inline]
    unsafe fn entry_mut(&mut self, index: usize) -> &mut Entry<K, V> {
        &mut *self.entries.as_mut_ptr().add(index)
    }

    #[inline]
    fn find<Q: ?Sized>(&self, k: &Q) -> Option<usize>
    where
        K: Borrow<Q>,
        Q: Hash + Eq,
    {
        self.index.index(k).and_then(|i| {
            let entry = unsafe { self.entry(i) };
            if entry.key.borrow() == k {
                Some(i)
            } else {
                None
            }
        })
    }

    /// Finds the key-value entry for a key.
    #[inline]
    pub fn get_key_value<Q: ?Sized>(&self, k: &Q) -> Option<(&K, &V)>
    where
        K: Borrow<Q>,
        Q: Hash + Eq,
    {
        self.find(k).map(move |index| {
            let entry = unsafe { self.entry(index) };
            (&entry.key, &entry.value)
        })
    }

    /// Finds the mutable key-value entry for a key.
    #[inline]
    pub fn get_key_value_pin<Q: ?Sized>(self: Pin<&mut Self>, k: &Q) -> Option<(&K, Pin<&mut V>)>
    where
        K: Borrow<Q>,
        Q: Hash + Eq,
    {
        unsafe {
            let hash_map = self.get_unchecked_mut();
            hash_map.find(k).map(move |index| {
                let entry = hash_map.entry_mut(index);
                (&entry.key, Pin::new_unchecked(&mut entry.value))
            })
        }
    }

    /// Returns whether a key is present in the hash map.
    #[inline]
    pub fn contains_key<Q: ?Sized>(&self, k: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq,
    {
        self.find(k).is_some()
    }

    /// Gets the value associated with the given key.
    #[inline]
    pub fn get<Q: ?Sized>(&self, k: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq,
    {
        self.find(k)
            .map(|index| unsafe { &self.entry(index).value })
    }

    /// Gets the mutable value associated with the given key.
    #[inline]
    pub fn get_pin<Q: ?Sized>(self: Pin<&mut Self>, k: &Q) -> Option<Pin<&mut V>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq,
    {
        unsafe {
            let hash_map = self.get_unchecked_mut();
            hash_map
                .find(k)
                .map(move |index| Pin::new_unchecked(&mut hash_map.entry_mut(index).value))
        }
    }

    /// Returns `true` if the map contains no elements.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    fn raw_iter(&self) -> RawIter<K, V> {
        RawIter::new(self.entries.as_ptr().cast(), self.len())
    }

    #[inline]
    fn raw_iter_pin(self: Pin<&mut Self>) -> RawIterPin<K, V> {
        unsafe {
            let hash_map = self.get_unchecked_mut();
            RawIterPin::new(hash_map.entries.as_mut_ptr().cast(), hash_map.len())
        }
    }

    /// Gets an iterator over the key-value entries in the hash map.
    #[inline]
    pub fn iter(&self) -> Iter<K, V> {
        Iter {
            inner: self.raw_iter(),
        }
    }

    /// Gets an iterator over the mutable key-value entries in the hash map.
    #[inline]
    pub fn iter_pin(self: Pin<&mut Self>) -> IterPin<K, V> {
        IterPin {
            inner: self.raw_iter_pin(),
        }
    }

    /// Gets an iterator over the keys in the hash map.
    #[inline]
    pub fn keys(&self) -> Keys<K, V> {
        Keys {
            inner: self.raw_iter(),
        }
    }

    /// Gets an iterator over the values in the hash map.
    #[inline]
    pub fn values(&self) -> Values<K, V> {
        Values {
            inner: self.raw_iter(),
        }
    }

    /// Gets an iterator over the mutable values in the hash map.
    #[inline]
    pub fn values_pin(self: Pin<&mut Self>) -> ValuesPin<K, V> {
        ValuesPin {
            inner: self.raw_iter_pin(),
        }
    }

    /// Resolves an archived hash map from a given length and parameters.
    ///
    /// # Safety
    ///
    /// - `len` must be the number of elements that were serialized
    /// - `pos` must be the position of `out` within the archive
    /// - `resolver` must be the result of serializing a hash map
    #[inline]
    pub unsafe fn resolve_from_len(
        len: usize,
        pos: usize,
        resolver: HashMapResolver,
        out: *mut Self,
    ) {
        let (fp, fo) = out_field!(out.index);
        ArchivedHashIndex::resolve_from_len(len, pos + fp, resolver.index_resolver, fo);

        let (fp, fo) = out_field!(out.entries);
        RelPtr::emplace(pos + fp, resolver.entries_pos, fo);
    }
}

#[cfg(feature = "alloc")]
const _: () = {
    impl<K, V> ArchivedHashMap<K, V> {
        /// Serializes an iterator of key-value pairs as a hash map.
        ///
        /// # Safety
        ///
        /// The keys returned by the iterator must be unique.
        pub unsafe fn serialize_from_iter<'a, KU, VU, S, I>(
            iter: I,
            serializer: &mut S,
        ) -> Result<HashMapResolver, S::Error>
        where
            KU: 'a + Serialize<S, Archived = K> + Hash + Eq,
            VU: 'a + Serialize<S, Archived = V>,
            S: Serializer + ScratchSpace + ?Sized,
            I: ExactSizeIterator<Item = (&'a KU, &'a VU)>,
        {
            use crate::ScratchVec;

            let len = iter.len();

            let mut entries = ScratchVec::new(serializer, len)?;
            entries.set_len(len);
            let index_resolver =
                ArchivedHashIndex::build_and_serialize(iter, serializer, &mut entries)?;
            let mut entries = entries.assume_init();

            // Serialize entries
            let mut resolvers = ScratchVec::new(serializer, len)?;
            for (key, value) in entries.iter() {
                resolvers.push((key.serialize(serializer)?, value.serialize(serializer)?));
            }

            let entries_pos = serializer.align_for::<Entry<K, V>>()?;
            for ((key, value), (key_resolver, value_resolver)) in
                entries.drain(..).zip(resolvers.drain(..))
            {
                serializer
                    .resolve_aligned(&Entry { key, value }, (key_resolver, value_resolver))?;
            }

            // Free scratch vecs
            resolvers.free(serializer)?;
            entries.free(serializer)?;

            Ok(HashMapResolver {
                index_resolver,
                entries_pos,
            })
        }
    }
};

impl<K: fmt::Debug, V: fmt::Debug> fmt::Debug for ArchivedHashMap<K, V> {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<K: Hash + Eq, V: Eq> Eq for ArchivedHashMap<K, V> {}

impl<K: Eq + Hash + Borrow<Q>, Q: Eq + Hash + ?Sized, V> Index<&'_ Q> for ArchivedHashMap<K, V> {
    type Output = V;

    #[inline]
    fn index(&self, key: &Q) -> &V {
        self.get(key).unwrap()
    }
}

impl<K: Hash + Eq, V: PartialEq> PartialEq for ArchivedHashMap<K, V> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        if self.len() != other.len() {
            false
        } else {
            self.iter()
                .all(|(key, value)| other.get(key).map_or(false, |v| *value == *v))
        }
    }
}

struct RawIter<'a, K, V> {
    current: *const Entry<K, V>,
    remaining: usize,
    _phantom: PhantomData<(&'a K, &'a V)>,
}

unsafe impl<'a, K, V> Send for RawIter<'a, K, V> {}

impl<'a, K, V> RawIter<'a, K, V> {
    #[inline]
    fn new(pairs: *const Entry<K, V>, len: usize) -> Self {
        Self {
            current: pairs,
            remaining: len,
            _phantom: PhantomData,
        }
    }
}

impl<'a, K, V> Iterator for RawIter<'a, K, V> {
    type Item = *const Entry<K, V>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        unsafe {
            if self.remaining == 0 {
                None
            } else {
                let result = self.current;
                self.current = self.current.add(1);
                self.remaining -= 1;
                Some(result)
            }
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<'a, K, V> ExactSizeIterator for RawIter<'a, K, V> {}
impl<'a, K, V> FusedIterator for RawIter<'a, K, V> {}

struct RawIterPin<'a, K, V> {
    current: *mut Entry<K, V>,
    remaining: usize,
    _phantom: PhantomData<(&'a K, Pin<&'a mut V>)>,
}

impl<'a, K, V> RawIterPin<'a, K, V> {
    #[inline]
    fn new(pairs: *mut Entry<K, V>, len: usize) -> Self {
        Self {
            current: pairs,
            remaining: len,
            _phantom: PhantomData,
        }
    }
}

impl<'a, K, V> Iterator for RawIterPin<'a, K, V> {
    type Item = *mut Entry<K, V>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        unsafe {
            if self.remaining == 0 {
                None
            } else {
                let result = self.current;
                self.current = self.current.add(1);
                self.remaining -= 1;
                Some(result)
            }
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<K, V> ExactSizeIterator for RawIterPin<'_, K, V> {}
impl<K, V> FusedIterator for RawIterPin<'_, K, V> {}

/// An iterator over the key-value pairs of a hash map.
#[repr(transparent)]
pub struct Iter<'a, K, V> {
    inner: RawIter<'a, K, V>,
}

impl<'a, K, V> Iterator for Iter<'a, K, V> {
    type Item = (&'a K, &'a V);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|x| unsafe {
            let pair = &*x;
            (&pair.key, &pair.value)
        })
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<K, V> ExactSizeIterator for Iter<'_, K, V> {}
impl<K, V> FusedIterator for Iter<'_, K, V> {}

/// An iterator over the mutable key-value pairs of a hash map.
#[repr(transparent)]
pub struct IterPin<'a, K, V> {
    inner: RawIterPin<'a, K, V>,
}

impl<'a, K, V> Iterator for IterPin<'a, K, V> {
    type Item = (&'a K, Pin<&'a mut V>);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|x| unsafe {
            let pair = &mut *x;
            (&pair.key, Pin::new_unchecked(&mut pair.value))
        })
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<K, V> ExactSizeIterator for IterPin<'_, K, V> {}
impl<K, V> FusedIterator for IterPin<'_, K, V> {}

/// An iterator over the keys of a hash map.
#[repr(transparent)]
pub struct Keys<'a, K, V> {
    inner: RawIter<'a, K, V>,
}

impl<'a, K, V> Iterator for Keys<'a, K, V> {
    type Item = &'a K;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|x| unsafe {
            let pair = &*x;
            &pair.key
        })
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<K, V> ExactSizeIterator for Keys<'_, K, V> {}
impl<K, V> FusedIterator for Keys<'_, K, V> {}

/// An iterator over the values of a hash map.
#[repr(transparent)]
pub struct Values<'a, K, V> {
    inner: RawIter<'a, K, V>,
}

impl<'a, K, V> Iterator for Values<'a, K, V> {
    type Item = &'a V;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|x| unsafe {
            let pair = &*x;
            &pair.value
        })
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<K, V> ExactSizeIterator for Values<'_, K, V> {}
impl<K, V> FusedIterator for Values<'_, K, V> {}

/// An iterator over the mutable values of a hash map.
#[repr(transparent)]
pub struct ValuesPin<'a, K, V> {
    inner: RawIterPin<'a, K, V>,
}

impl<'a, K, V> Iterator for ValuesPin<'a, K, V> {
    type Item = Pin<&'a mut V>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|x| unsafe {
            let pair = &mut *x;
            Pin::new_unchecked(&mut pair.value)
        })
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<K, V> ExactSizeIterator for ValuesPin<'_, K, V> {}
impl<K, V> FusedIterator for ValuesPin<'_, K, V> {}

/// The resolver for archived hash maps.
pub struct HashMapResolver {
    index_resolver: HashIndexResolver,
    entries_pos: usize,
}

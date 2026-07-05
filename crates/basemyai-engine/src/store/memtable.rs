//! In-memory sorted table backing the write path. A `BTreeMap` keeps
//! iteration order == key order for free, which `store::sst::SstFile::write_new`
//! relies on when it turns a flushed memtable into a sorted SST file.

use std::collections::BTreeMap;

use crate::key::Key;
use crate::store::Value;

#[derive(Debug, Default)]
pub(crate) struct Memtable {
    /// `None` = tombstone (an explicit delete recorded at this layer).
    entries: BTreeMap<Key, Option<Value>>,
}

impl Memtable {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn put(&mut self, key: Key, value: Value) {
        self.entries.insert(key, Some(value));
    }

    pub(crate) fn delete(&mut self, key: Key) {
        self.entries.insert(key, None);
    }

    /// `None` = key absent from this memtable (caller must check SSTs next).
    /// `Some(None)` = tombstone (definitively deleted, do not check SSTs).
    /// `Some(Some(v))` = present.
    pub(crate) fn get(&self, key: &Key) -> Option<Option<&Value>> {
        self.entries.get(key).map(|v| v.as_ref())
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    /// Iterates in ascending key order — already sorted, no extra work.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&Key, &Option<Value>)> {
        self.entries.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_then_get() {
        let mut m = Memtable::new();
        m.put(Key::from(&b"k"[..]), b"v".to_vec());
        assert_eq!(m.get(&Key::from(&b"k"[..])), Some(Some(&b"v".to_vec())));
    }

    #[test]
    fn delete_records_tombstone() {
        let mut m = Memtable::new();
        m.put(Key::from(&b"k"[..]), b"v".to_vec());
        m.delete(Key::from(&b"k"[..]));
        assert_eq!(m.get(&Key::from(&b"k"[..])), Some(None));
    }

    #[test]
    fn missing_key_is_outer_none() {
        let m = Memtable::new();
        assert_eq!(m.get(&Key::from(&b"missing"[..])), None);
    }

    #[test]
    fn iter_is_sorted_ascending() {
        let mut m = Memtable::new();
        m.put(Key::from(&b"b"[..]), b"2".to_vec());
        m.put(Key::from(&b"a"[..]), b"1".to_vec());
        let keys: Vec<_> = m.iter().map(|(k, _)| k.as_bytes().to_vec()).collect();
        assert_eq!(keys, vec![b"a".to_vec(), b"b".to_vec()]);
    }
}

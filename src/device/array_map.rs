#![allow(dead_code)]

#[derive(Clone, Copy, Debug)]
pub struct ArrayMap<K, V, const N: usize> {
    keys: [Option<K>; N],
    values: [V; N],
}

impl<K: Copy + PartialEq, V: Copy, const N: usize> ArrayMap<K, V, N> {
    pub fn new(keys: [Option<K>; N], values: [V; N]) -> Self {
        ArrayMap { keys, values }
    }

    pub fn get(&self, key: &K) -> Option<&V> {
        for i in 0..N {
            if let Some(k) = &self.keys[i] {
                if k == key {
                    return Some(&self.values[i]);
                }
            }
        }
        None
    }

    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        for i in 0..N {
            if let Some(k) = &self.keys[i] {
                if k == key {
                    return Some(&mut self.values[i]);
                }
            }
        }
        None
    }

    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        for i in 0..N {
            if let Some(k) = self.keys[i] {
                if k == key {
                    let old_value = self.values[i];
                    self.values[i] = value;
                    return Some(old_value);
                }
            }
        }
        for i in 0..N {
            if self.keys[i].is_none() {
                self.keys[i] = Some(key);
                self.values[i] = value;
                return None;
            }
        }
        None
    }

    pub fn contains_key(&self, key: &K) -> bool {
        for i in 0..N {
            if let Some(k) = &self.keys[i] {
                if k == key {
                    return true;
                }
            }
        }
        false
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        for i in 0..N {
            if let Some(k) = &self.keys[i] {
                if k == key {
                    self.keys[i] = None;
                    return Some(self.values[i]);
                }
            }
        }
        None
    }

    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.keys
            .iter()
            .zip(self.values.iter())
            .filter_map(|(k, v)| k.as_ref().map(|key| (key, v)))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&K, &mut V)> {
        self.keys
            .iter()
            .zip(self.values.iter_mut())
            .filter_map(|(k, v)| k.as_ref().map(|key| (key, v)))
    }

    pub fn len(&self) -> usize {
        self.keys.iter().filter(|k| k.is_some()).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<K: Copy + PartialEq, V: Copy, const N: usize> core::ops::Index<&K> for ArrayMap<K, V, N> {
    type Output = V;

    fn index(&self, key: &K) -> &Self::Output {
        self.get(key).expect("key not found in ArrayMap")
    }
}

impl<K: Copy + PartialEq, V: Copy, const N: usize> core::ops::IndexMut<&K> for ArrayMap<K, V, N> {
    fn index_mut(&mut self, key: &K) -> &mut Self::Output {
        self.get_mut(key).expect("key not found in ArrayMap")
    }
}

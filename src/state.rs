use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{VecDeque, vec_deque};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Stamped<T> {
    pub at: DateTime<Utc>,
    pub value: T,
}

impl<T> Stamped<T> {
    pub fn now(value: T) -> Self {
        Self {
            at: Utc::now(),
            value,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BoundedLog<T, const N: usize> {
    entries: VecDeque<T>,
}

impl<T, const N: usize> Default for BoundedLog<T, N> {
    fn default() -> Self {
        Self {
            entries: VecDeque::with_capacity(N),
        }
    }
}

impl<T, const N: usize> BoundedLog<T, N> {
    pub fn push(&mut self, value: T) {
        if N == 0 {
            return;
        }
        if self.entries.len() == N {
            self.entries.pop_front();
        }
        self.entries.push_back(value);
    }

    pub fn iter(&self) -> vec_deque::Iter<'_, T> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<'a, T, const N: usize> IntoIterator for &'a BoundedLog<T, N> {
    type Item = &'a T;
    type IntoIter = vec_deque::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

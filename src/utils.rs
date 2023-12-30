use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub struct ExpiredDeque<T> {
    inner: VecDeque<ExpiredElement<T>>,
    expire: Duration,
}

#[derive(Debug, Clone)]
struct ExpiredElement<T> {
    value: T,
    expire: Instant,
}

impl<T> ExpiredDeque<T> {
    pub const fn new(expire: Duration) -> Self {
        Self {
            inner: VecDeque::new(),
            expire,
        }
    }

    pub fn push_back(&mut self, value: T) {
        self.inner.push_back(ExpiredElement {
            value,
            expire: Instant::now() + self.expire,
        });
    }

    #[allow(unused)]
    pub fn pop_front(&mut self) -> Option<T> {
        let now = Instant::now();
        while let Some(item) = self.inner.pop_front() {
            if item.expire >= now {
                return Some(item.value);
            }
        }
        None
    }

    pub fn clean(&mut self) {
        let now = Instant::now();
        while let Some(item) = self.inner.front() {
            if item.expire < now {
                self.inner.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        let now = Instant::now();
        self.inner.iter().filter_map(move |item| {
            if item.expire >= now {
                Some(&item.value)
            } else {
                None
            }
        })
    }
}

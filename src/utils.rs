use std::{
    collections::{btree_map, BTreeMap, HashMap, VecDeque},
    time::{Duration, Instant},
};

use serde::{de::DeserializeOwned, Deserialize};
use teloxide::types::ReplyParameters;
use toml::Value;

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

pub const DEFAULT_SMM_NAME: &str = "__default__";

#[derive(Clone, Debug)]
pub enum SingleMultiMap<T> {
    Single(T),
    // Use BTreeMap to keep the order of the keys
    Multi(BTreeMap<String, T>),
}

// Note: if given hashmap is empty, it will panic
impl<T> From<HashMap<String, T>> for SingleMultiMap<T> {
    fn from(map: HashMap<String, T>) -> Self {
        if map.is_empty() {
            panic!("unexpected empty list");
        }
        if map.len() == 1 {
            let (_, config) = map.into_iter().next().unwrap();
            return Self::Single(config);
        }
        Self::Multi(map.into_iter().collect())
    }
}

impl<'de, T: DeserializeOwned> Deserialize<'de> for SingleMultiMap<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // note: here `Value` is `toml::Value`
        let value = Value::deserialize(deserializer)?;

        // try to parse as single config
        if let Ok(config) = T::deserialize(value.clone()) {
            return Ok(Self::Single(config));
        }

        // parse as multi config
        let config = BTreeMap::<String, T>::deserialize(value).map_err(serde::de::Error::custom)?;
        if config.is_empty() {
            return Err(serde::de::Error::custom("unexpected empty list"));
        }
        // if only one config, return as single config
        if config.len() == 1 {
            let (_, config) = config.into_iter().next().unwrap();
            return Ok(Self::Single(config));
        }
        Ok(Self::Multi(config))
    }
}

impl<T> IntoIterator for SingleMultiMap<T> {
    type Item = (String, T);
    type IntoIter = SingleMultiMapIntoIter<T>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        match self {
            Self::Single(inner) => SingleMultiMapIntoIter::Single(Some(inner)),
            Self::Multi(map) => SingleMultiMapIntoIter::Multi(map.into_iter()),
        }
    }
}

impl<T> SingleMultiMap<T> {
    pub fn get(&self, name: &str) -> Option<&T> {
        match self {
            Self::Single(inner) => Some(inner),
            Self::Multi(map) => map.get(name),
        }
    }

    pub fn unwrap_single_ref(&self) -> Option<&T> {
        match self {
            Self::Single(inner) => Some(inner),
            Self::Multi(_) => None,
        }
    }

    #[allow(unused)]
    pub fn unwrap_multi_ref(&self) -> Option<&BTreeMap<String, T>> {
        match self {
            Self::Single(_) => None,
            Self::Multi(inner) => Some(inner),
        }
    }

    #[inline]
    pub fn iter(&self) -> SingleMultiMapIter<'_, T> {
        match self {
            Self::Single(inner) => SingleMultiMapIter::Single(Some(inner)),
            Self::Multi(map) => SingleMultiMapIter::Multi(map.iter()),
        }
    }
}

pub enum SingleMultiMapIter<'a, T> {
    Single(Option<&'a T>),
    Multi(btree_map::Iter<'a, String, T>),
}

impl<'a, T> Iterator for SingleMultiMapIter<'a, T> {
    type Item = (&'a str, &'a T);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Single(inner) => inner.take().map(|inner| (DEFAULT_SMM_NAME, inner)),
            Self::Multi(iter) => iter.next().map(|(k, v)| (k.as_str(), v)),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::Single(inner) => {
                if inner.is_some() {
                    (1, Some(1))
                } else {
                    (0, Some(0))
                }
            }
            Self::Multi(iter) => iter.size_hint(),
        }
    }
}

pub enum SingleMultiMapIntoIter<T> {
    Single(Option<T>),
    Multi(btree_map::IntoIter<String, T>),
}

impl<T> Iterator for SingleMultiMapIntoIter<T> {
    type Item = (String, T);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Single(inner) => inner
                .take()
                .map(|inner| (DEFAULT_SMM_NAME.to_string(), inner)),
            Self::Multi(iter) => iter.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::Single(inner) => {
                if inner.is_some() {
                    (1, Some(1))
                } else {
                    (0, Some(0))
                }
            }
            Self::Multi(iter) => iter.size_hint(),
        }
    }
}

pub trait SendMessageSettersExt {
    fn reply_to_message_id_opt(self, message_id: Option<teloxide::types::MessageId>) -> Self;
}

impl<T: teloxide::payloads::SendMessageSetters> SendMessageSettersExt for T {
    fn reply_to_message_id_opt(self, message_id: Option<teloxide::types::MessageId>) -> Self {
        if let Some(message_id) = message_id {
            self.reply_parameters(ReplyParameters::new(message_id))
        } else {
            self
        }
    }
}

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
        let now = Instant::now();
        !self.inner.iter().any(|item| item.expire >= now)
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

#[derive(Debug)]
pub struct EmptyMapError;

impl std::fmt::Display for EmptyMapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cannot create SingleMultiMap from empty map")
    }
}

impl std::error::Error for EmptyMapError {}

impl<T> TryFrom<HashMap<String, T>> for SingleMultiMap<T> {
    type Error = EmptyMapError;

    fn try_from(map: HashMap<String, T>) -> Result<Self, Self::Error> {
        if map.is_empty() {
            return Err(EmptyMapError);
        }
        if map.len() == 1 {
            let (_, config) = map.into_iter().next().unwrap();
            return Ok(Self::Single(config));
        }
        Ok(Self::Multi(map.into_iter().collect()))
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

#[cfg(test)]
mod tests {
    use super::*;

    mod expired_deque {
        use super::*;

        #[test]
        fn test_push_and_iter() {
            let mut deque = ExpiredDeque::new(Duration::from_secs(10));
            deque.push_back(1);
            deque.push_back(2);
            deque.push_back(3);

            let items: Vec<_> = deque.iter().copied().collect();
            assert_eq!(items, vec![1, 2, 3]);
        }

        #[test]
        fn test_is_empty() {
            let deque: ExpiredDeque<i32> = ExpiredDeque::new(Duration::from_secs(10));
            assert!(deque.is_empty());

            let mut deque = ExpiredDeque::new(Duration::from_secs(10));
            deque.push_back(1);
            assert!(!deque.is_empty());
        }

        #[test]
        fn test_expired_items_not_returned() {
            let mut deque = ExpiredDeque::new(Duration::from_millis(1));
            deque.push_back(1);
            std::thread::sleep(Duration::from_millis(10));

            assert!(deque.is_empty());
            assert_eq!(deque.iter().count(), 0);
        }

        #[test]
        fn test_pop_front() {
            let mut deque = ExpiredDeque::new(Duration::from_secs(10));
            deque.push_back(1);
            deque.push_back(2);

            assert_eq!(deque.pop_front(), Some(1));
            assert_eq!(deque.pop_front(), Some(2));
            assert_eq!(deque.pop_front(), None);
        }

        #[test]
        fn test_clean() {
            let mut deque = ExpiredDeque::new(Duration::from_millis(1));
            deque.push_back(1);
            std::thread::sleep(Duration::from_millis(10));
            deque.push_back(2);

            deque.clean();
            assert_eq!(deque.inner.len(), 1);
        }
    }

    mod single_multi_map {
        use super::*;

        #[test]
        fn test_try_from_empty_map() {
            let map: HashMap<String, i32> = HashMap::new();
            assert!(SingleMultiMap::try_from(map).is_err());
        }

        #[test]
        fn test_try_from_single_item() {
            let mut map = HashMap::new();
            map.insert("key".to_string(), 42);

            let smm = SingleMultiMap::try_from(map).unwrap();
            assert!(matches!(smm, SingleMultiMap::Single(42)));
        }

        #[test]
        fn test_try_from_multiple_items() {
            let mut map = HashMap::new();
            map.insert("a".to_string(), 1);
            map.insert("b".to_string(), 2);

            let smm = SingleMultiMap::try_from(map).unwrap();
            assert!(matches!(smm, SingleMultiMap::Multi(_)));
        }

        #[test]
        fn test_get_single() {
            let smm = SingleMultiMap::Single(42);
            assert_eq!(smm.get("any_key"), Some(&42));
        }

        #[test]
        fn test_get_multi() {
            let mut map = BTreeMap::new();
            map.insert("a".to_string(), 1);
            map.insert("b".to_string(), 2);
            let smm = SingleMultiMap::Multi(map);

            assert_eq!(smm.get("a"), Some(&1));
            assert_eq!(smm.get("b"), Some(&2));
            assert_eq!(smm.get("c"), None);
        }

        #[test]
        fn test_iter_single() {
            let smm = SingleMultiMap::Single(42);
            let items: Vec<_> = smm.iter().collect();
            assert_eq!(items, vec![(DEFAULT_SMM_NAME, &42)]);
        }

        #[test]
        fn test_iter_multi() {
            let mut map = BTreeMap::new();
            map.insert("a".to_string(), 1);
            map.insert("b".to_string(), 2);
            let smm = SingleMultiMap::Multi(map);

            let items: Vec<_> = smm.iter().collect();
            assert_eq!(items, vec![("a", &1), ("b", &2)]);
        }

        #[test]
        fn test_into_iter() {
            let smm = SingleMultiMap::Single(42);
            let items: Vec<_> = smm.into_iter().collect();
            assert_eq!(items, vec![(DEFAULT_SMM_NAME.to_string(), 42)]);
        }

        #[test]
        fn test_unwrap_single_ref() {
            let smm = SingleMultiMap::Single(42);
            assert_eq!(smm.unwrap_single_ref(), Some(&42));

            let smm = SingleMultiMap::Multi(BTreeMap::new());
            assert_eq!(smm.unwrap_single_ref(), None::<&()>);
        }
    }
}

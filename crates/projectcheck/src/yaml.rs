//! YAML loading that stays bounded on hostile input.
//!
//! The parser's own alias limit counts jumps, not nodes, so one wide list
//! repeated by many aliases passes that limit and still expands to billions of
//! nodes. A first pass therefore walks the document with a node budget and
//! keeps nothing in memory. The value is built only after that pass succeeds.

use std::cell::Cell;
use std::fmt;

use serde::de::{self, DeserializeSeed, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor};
use serde_norway::{Deserializer, Mapping, Value};

use crate::{clean, Error};

/// The most nodes a document may expand to. Real Compose files have a few
/// thousand at most.
pub(crate) const MAX_NODES: u64 = 250_000;

/// The limit on `[` and `{` characters times the file size. The YAML scanner
/// does work proportional to the flow nesting depth for every token, so deep
/// nesting takes quadratic time before any other limit applies: 100,000 `[`
/// took 35 seconds in a debug build. Depth cannot exceed the number of
/// openers, and tokens cannot exceed the number of bytes, so this product caps
/// that work no matter how quotes or comments are arranged. It allows 2,000
/// openers in a 1 MiB file.
const MAX_FLOW_WORK: u64 = 2_000 * 1024 * 1024;

pub(crate) fn load(text: &str) -> Result<Value, Error> {
    load_with_budget(text, MAX_NODES)
}

fn load_with_budget(text: &str, limit: u64) -> Result<Value, Error> {
    let openers = text.bytes().filter(|b| matches!(b, b'[' | b'{')).count() as u64;
    if openers.saturating_mul(text.len() as u64) > MAX_FLOW_WORK {
        return Err(Error::TooManyFlowCollections);
    }
    let count = Cell::new(0);
    let seed = Counter {
        count: &count,
        limit,
    };
    if let Err(err) = seed.deserialize(Deserializer::from_str(text)) {
        if count.get() > limit {
            return Err(Error::TooComplex { limit });
        }
        return Err(Error::Yaml(clean(&err.to_string())));
    }
    let mut value: Value =
        serde_norway::from_str(text).map_err(|err| Error::Yaml(clean(&err.to_string())))?;
    resolve_merges(&mut value)?;
    Ok(value)
}

/// True if any value in the tree has a YAML tag.
pub(crate) fn has_tag(value: &Value) -> bool {
    match value {
        Value::Tagged(_) => true,
        Value::Sequence(items) => items.iter().any(has_tag),
        Value::Mapping(map) => map.iter().any(|(k, v)| has_tag(k) || has_tag(v)),
        _ => false,
    }
}

#[derive(Clone, Copy)]
struct Counter<'c> {
    count: &'c Cell<u64>,
    limit: u64,
}

impl Counter<'_> {
    fn tick<E: de::Error>(&self) -> Result<(), E> {
        let next = self.count.get().saturating_add(1);
        self.count.set(next);
        if next > self.limit {
            return Err(E::custom("node budget exceeded"));
        }
        Ok(())
    }
}

impl<'de> DeserializeSeed<'de> for Counter<'_> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Counter<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any YAML value")
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<(), E> {
        self.tick()
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<(), E> {
        self.tick()
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<(), E> {
        self.tick()
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<(), E> {
        self.tick()
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<(), E> {
        self.tick()
    }

    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        self.tick()
    }

    fn visit_none<E: de::Error>(self) -> Result<(), E> {
        self.tick()
    }

    fn visit_some<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        self.tick()?;
        self.deserialize(deserializer)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        self.tick()?;
        while seq.next_element_seed(self)?.is_some() {}
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        self.tick()?;
        while map.next_entry_seed(self, self)?.is_some() {}
        Ok(())
    }

    fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> Result<(), A::Error> {
        self.tick()?;
        let (_tag, contents) = data.variant::<String>()?;
        contents.newtype_variant_seed(self)
    }
}

/// Applies `<<` merge keys, innermost first, so that a merge of a mapping that
/// itself has a merge is not left behind. The crate's `apply_merge` leaves a
/// nested `<<` in that case, which would hide its contents from the checks.
fn resolve_merges(value: &mut Value) -> Result<(), Error> {
    match value {
        Value::Mapping(map) => {
            for (_, child) in map.iter_mut() {
                resolve_merges(child)?;
            }
            if let Some(merge) = map.shift_remove("<<") {
                apply_merge(map, merge)?;
            }
            Ok(())
        }
        Value::Sequence(items) => items.iter_mut().try_for_each(resolve_merges),
        Value::Tagged(tagged) => resolve_merges(&mut tagged.value),
        _ => Ok(()),
    }
}

fn apply_merge(map: &mut Mapping, merge: Value) -> Result<(), Error> {
    let sources = match merge {
        Value::Mapping(source) => vec![source],
        Value::Sequence(items) => items
            .into_iter()
            .map(|item| match item {
                Value::Mapping(source) => Ok(source),
                _ => Err(merge_shape_error()),
            })
            .collect::<Result<_, _>>()?,
        _ => return Err(merge_shape_error()),
    };
    for source in sources {
        for (key, item) in source {
            map.entry(key).or_insert(item);
        }
    }
    Ok(())
}

fn merge_shape_error() -> Error {
    Error::Yaml("a << merge must be a mapping or a list of mappings".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_document_is_null() {
        assert_eq!(load("").unwrap(), Value::Null);
    }

    #[test]
    fn chained_merges_are_flattened() {
        let value = load("a: &a {x: 1}\nb: &b {<<: *a}\nc: {<<: *b, y: 2}\n").unwrap();
        let c = value.get("c").unwrap();
        assert!(c.get("<<").is_none());
        assert_eq!(c.get("x").and_then(Value::as_u64), Some(1));
        assert_eq!(c.get("y").and_then(Value::as_u64), Some(2));
    }

    #[test]
    fn explicit_keys_beat_merged_keys() {
        let value = load("a: &a {x: 1}\nc: {x: 2, <<: *a}\n").unwrap();
        assert_eq!(value.get("c").unwrap().get("x").unwrap().as_u64(), Some(2));
    }

    #[test]
    fn scalar_merge_is_an_error() {
        assert!(matches!(load("c: {<<: 5}\n"), Err(Error::Yaml(_))));
    }

    #[test]
    fn budget_counts_expanded_nodes() {
        let text = "a: &a [1, 2, 3]\nb: [*a, *a, *a, *a]\n";
        assert!(load_with_budget(text, 1_000).is_ok());
        assert!(matches!(
            load_with_budget(text, 5),
            Err(Error::TooComplex { limit: 5 })
        ));
    }

    #[test]
    fn duplicate_keys_are_an_error() {
        assert!(load("a: 1\na: 2\n").is_err());
    }

    #[test]
    fn multiple_documents_are_an_error() {
        assert!(load("a: 1\n---\nb: 2\n").is_err());
    }
}

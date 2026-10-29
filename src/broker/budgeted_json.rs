//! A render service answer read with a memory budget. Reading JSON into `serde_json::Value` builds a value for every
//! element, which can take many times the answer's own size: 160 MiB of one-key objects is several GiB of values, more
//! than the worker has. This reader keeps only the top-level fields its caller names and skips the others without
//! building them. It charges each value and key it keeps with what holding it costs before it is copied, and stops as
//! soon as the budget is spent, so a large answer is refused while it is read, not after it has been held. Besides the
//! answer and what it keeps, the parser holds one text at a time in its scratch room when the text has escapes: at most
//! the answer's size.
use std::{cell::Cell, fmt};

use serde::de::{self, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};

/// Why an answer was not read.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// Not a JSON object, or values nested deeper than `MAX_DEPTH`.
    Unreadable,
    /// The kept fields cost more than the budget.
    TooLarge,
}

/// What holding a kept value costs, in bytes, besides the text it carries: an item of an array (its `Value` and the
/// room the array keeps to grow), an entry of an object (its share of a tree node and its key's allocation), and a
/// text's allocation.
const ITEM: usize = 64;
const ENTRY: usize = 128;
const TEXT: usize = 32;
/// Kept values nest at most this deep.
pub const MAX_DEPTH: usize = 32;

/// The answer's fields named in `keep`, as an object, read within `budget` bytes of held values. Other fields are read
/// past and cost nothing.
pub fn read(bytes: &[u8], keep: &[&str], budget: usize) -> Result<Value, Refused> {
    let charges = Charges {
        left: Cell::new(budget),
        spent: Cell::new(false),
    };
    let mut reader = serde_json::Deserializer::from_slice(bytes);
    let read = Fields {
        keep,
        charges: &charges,
    }
    .deserialize(&mut reader)
    .and_then(|value| reader.end().map(|()| value));
    match read {
        Ok(value) => Ok(value),
        Err(_) if charges.spent.get() => Err(Refused::TooLarge),
        Err(_) => Err(Refused::Unreadable),
    }
}

struct Charges {
    left: Cell<usize>,
    spent: Cell<bool>,
}

impl Charges {
    fn charge<E: de::Error>(&self, cost: usize) -> Result<(), E> {
        match self.left.get().checked_sub(cost) {
            Some(left) => {
                self.left.set(left);
                Ok(())
            }
            None => {
                self.spent.set(true);
                Err(E::custom("budget_spent"))
            }
        }
    }
}

/// The top-level object: kept fields are held, others skipped.
struct Fields<'a> {
    keep: &'a [&'a str],
    charges: &'a Charges,
}

impl<'de> DeserializeSeed<'de> for Fields<'_> {
    type Value = Value;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for Fields<'_> {
    type Value = Value;
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a JSON object")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut kept = Map::new();
        while let Some(key) = map.next_key_seed(Key {
            keep: Some(self.keep),
            charges: self.charges,
        })? {
            match key {
                Some(key) => {
                    let value = map.next_value_seed(Held {
                        depth: 1,
                        charges: self.charges,
                    })?;
                    kept.insert(key, value);
                }
                None => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(Value::Object(kept))
    }
}

/// An object's key, copied only when it is kept (`keep` names it, or is none) and charged first. The parser hands over
/// the key's text borrowed from the answer or from its scratch room, so a key that is skipped or would spend the budget
/// is never copied.
struct Key<'a> {
    keep: Option<&'a [&'a str]>,
    charges: &'a Charges,
}

impl<'de> DeserializeSeed<'de> for Key<'_> {
    type Value = Option<String>;
    fn deserialize<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Option<String>, D::Error> {
        deserializer.deserialize_str(self)
    }
}

impl<'de> Visitor<'de> for Key<'_> {
    type Value = Option<String>;
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("an object key")
    }
    fn visit_str<E: de::Error>(self, key: &str) -> Result<Option<String>, E> {
        if self.keep.is_some_and(|keep| !keep.contains(&key)) {
            return Ok(None);
        }
        self.charges.charge(ENTRY + key.len())?;
        Ok(Some(key.to_owned()))
    }
}

/// One kept value at `depth`.
struct Held<'a> {
    depth: usize,
    charges: &'a Charges,
}

impl Held<'_> {
    fn below(&self) -> Self {
        Held {
            depth: self.depth + 1,
            charges: self.charges,
        }
    }
    fn nest<E: de::Error>(&self) -> Result<(), E> {
        if self.depth >= MAX_DEPTH {
            return Err(E::custom("nested_too_deep"));
        }
        Ok(())
    }
}

impl<'de> DeserializeSeed<'de> for Held<'_> {
    type Value = Value;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Held<'_> {
    type Value = Value;
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a JSON value")
    }
    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }
    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(Value::from(value))
    }
    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(Value::from(value))
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("number_not_finite"))
    }
    fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
        self.charges.charge(TEXT + value.len())?;
        Ok(Value::String(value.to_owned()))
    }
    fn visit_string<E: de::Error>(self, value: String) -> Result<Value, E> {
        self.charges.charge(TEXT + value.len())?;
        Ok(Value::String(value))
    }
    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_none<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        self.nest()?;
        let mut items = Vec::new();
        loop {
            // Charged before each item is read, so a long run of small items stops at the budget.
            self.charges.charge(ITEM)?;
            match seq.next_element_seed(self.below())? {
                Some(item) => items.push(item),
                None => break,
            }
        }
        Ok(Value::Array(items))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        self.nest()?;
        let mut entries = Map::new();
        while let Some(key) = map.next_key_seed(Key {
            keep: None,
            charges: self.charges,
        })? {
            let value = map.next_value_seed(self.below())?;
            if let Some(key) = key {
                entries.insert(key, value);
            }
        }
        Ok(Value::Object(entries))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn kept_fields_read_as_serde_json_reads_them_and_others_are_left_out() {
        let answer = json!({"results_exact": {"columns": ["a", "b"], "rows": [{"a": 1, "b": "x"}, {"a": -2.5e-30, "b": null}],
            "stats": {"p": 0.000123456789012345, "ok": true}}, "png": "iVBOR", "csv": "a,b\n1,x\n", "ms": 3});
        let bytes = serde_json::to_vec(&answer).unwrap();
        let kept = read(&bytes, &["results_exact", "png", "absent"], 1 << 20).unwrap();
        assert_eq!(
            kept,
            json!({"results_exact": answer["results_exact"], "png": "iVBOR"})
        );
    }

    #[test]
    fn a_skipped_field_costs_nothing() {
        let big = "x".repeat(1 << 20);
        let bytes = serde_json::to_vec(&json!({"csv": big, "rows": [[1, 2]], "n": 1})).unwrap();
        assert_eq!(read(&bytes, &["n"], ENTRY + 1).unwrap(), json!({"n": 1}));
        assert_eq!(
            read(&bytes, &["csv"], 1 << 16).unwrap_err(),
            Refused::TooLarge
        );
    }

    #[test]
    fn many_small_rows_are_refused_at_the_budget_and_not_before() {
        let rows: Vec<Value> = (0..1000).map(|i| json!({"a": i})).collect();
        let bytes = serde_json::to_vec(&json!({"rows": rows})).unwrap();
        // The field, one charge per item and one for the end, and one entry per row.
        let exact = ENTRY + 4 + 1001 * ITEM + 1000 * (ENTRY + 1);
        assert_eq!(
            read(&bytes, &["rows"], exact).unwrap()["rows"][999]["a"],
            999
        );
        assert_eq!(
            read(&bytes, &["rows"], exact - 1).unwrap_err(),
            Refused::TooLarge
        );
    }

    #[test]
    fn a_kept_key_is_charged_by_its_length() {
        let bytes = br#"{"rows":{"abcdef":null}}"#;
        let exact = ENTRY + 4 + ENTRY + 6;
        assert_eq!(
            read(bytes, &["rows"], exact).unwrap()["rows"],
            json!({"abcdef": null})
        );
        assert_eq!(
            read(bytes, &["rows"], exact - 1).unwrap_err(),
            Refused::TooLarge
        );
        // An escaped key is charged by its text.
        let escaped = br#"{"rows":{"ab\u0063":null}}"#;
        assert_eq!(
            read(escaped, &["rows"], ENTRY + 4 + ENTRY + 3).unwrap()["rows"],
            json!({"abc": null})
        );
    }

    #[test]
    fn text_is_charged_by_its_length() {
        let bytes = br#"{"svg":"<svg/>"}"#;
        let exact = ENTRY + 3 + TEXT + 6;
        assert_eq!(read(bytes, &["svg"], exact).unwrap()["svg"], "<svg/>");
        assert_eq!(
            read(bytes, &["svg"], exact - 1).unwrap_err(),
            Refused::TooLarge
        );
    }

    #[test]
    fn what_is_not_one_json_object_is_unreadable() {
        for bytes in [
            &b"[1, 2]"[..],
            b"{\"a\": 1",
            b"{\"a\": 1} {}",
            b"{\"a\": 1e400}",
            b"",
        ] {
            assert_eq!(
                read(bytes, &["a"], 1 << 20).unwrap_err(),
                Refused::Unreadable,
                "{}",
                String::from_utf8_lossy(bytes)
            );
        }
    }

    #[test]
    fn values_nest_at_most_max_depth() {
        let nested =
            |depth: usize| format!("{{\"a\": {}1{}}}", "[".repeat(depth), "]".repeat(depth));
        assert!(read(nested(MAX_DEPTH - 1).as_bytes(), &["a"], 1 << 20).is_ok());
        assert_eq!(
            read(nested(MAX_DEPTH).as_bytes(), &["a"], 1 << 20).unwrap_err(),
            Refused::Unreadable
        );
        // A skipped field is not held, so its depth is serde_json's to bound.
        assert!(read(nested(MAX_DEPTH + 10).as_bytes(), &["b"], 1 << 20).is_ok());
    }
}

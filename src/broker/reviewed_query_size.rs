//! The size PostgreSQL gives a definition. Migration 071 stores a registry row only while
//! `octet_length(definition::text)` is at most 65,536 bytes; `registration_check` applies the same bound so `render`
//! never prints a body `register` cannot store.
use serde_json::Value;

/// The registry's limit on a stored definition's jsonb text (migration 071).
pub const MAX_DEFINITION_BYTES: usize = 65536;

/// The bytes of `value` as PostgreSQL prints it from jsonb: `", "` between items, `": "` after keys, strings escaped as
/// serde_json escapes them, and numbers in numeric's plain decimal form, with any exponent written out.
pub fn jsonb_text_bytes(value: &Value) -> usize {
    match value {
        Value::Null => 4,
        Value::Bool(true) => 4,
        Value::Bool(false) => 5,
        Value::Number(number) => numeric_text_bytes(&number.to_string()),
        Value::String(text) => string_bytes(text),
        Value::Array(items) => {
            2 + items.iter().map(jsonb_text_bytes).sum::<usize>()
                + 2 * items.len().saturating_sub(1)
        }
        Value::Object(map) => {
            2 + map
                .iter()
                .map(|(key, item)| string_bytes(key) + 2 + jsonb_text_bytes(item))
                .sum::<usize>()
                + 2 * map.len().saturating_sub(1)
        }
    }
}

fn string_bytes(text: &str) -> usize {
    serde_json::to_string(text).map_or(usize::MAX / 4, |quoted| quoted.len())
}

/// numeric's text for a JSON number literal. numeric keeps the literal's scale (digits after the point less the
/// exponent, at least zero) and writes every integer digit, so `1e-7` prints as `0.0000001` and `1e308` as 309 digits.
fn numeric_text_bytes(literal: &str) -> usize {
    let (mantissa, exponent) = match literal.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i64>().unwrap_or(0)),
        None => (literal, 0),
    };
    let negative = mantissa.starts_with('-');
    let digits = mantissa.trim_start_matches(['-', '+']);
    let (integer, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    let all: String = integer.chars().chain(fraction.chars()).collect();
    let leading_zeros = all.chars().take_while(|c| *c == '0').count();
    let zero = leading_zeros == all.len();
    // The decimal point sits after this many digits of `all`, counted from its first significant digit.
    let point = integer.len() as i64 + exponent - leading_zeros as i64;
    let integer_digits = if zero || point <= 0 {
        1
    } else {
        point as usize
    };
    let scale = (fraction.len() as i64 - exponent).max(0) as usize;
    usize::from(negative && !zero) + integer_digits + if scale > 0 { 1 + scale } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn numbers_print_as_numeric_does() {
        for (literal, text) in [
            ("0.25", "0.25"),
            ("100.0", "100.0"),
            ("1e-7", "0.0000001"),
            ("1.5e3", "1500"),
            ("-5", "-5"),
            ("0", "0"),
            ("0.0", "0.0"),
            ("-0.0", "0.0"),
            ("12.5e-1", "1.25"),
        ] {
            assert_eq!(numeric_text_bytes(literal), text.len(), "{literal}");
        }
        assert_eq!(numeric_text_bytes("1.7976931348623157e308"), 309);
    }

    #[test]
    fn containers_use_postgres_separators() {
        assert_eq!(
            jsonb_text_bytes(&json!({"a": [1, null], "b": true})),
            r#"{"a": [1, null], "b": true}"#.len()
        );
        assert_eq!(jsonb_text_bytes(&json!({})), 2);
        assert_eq!(jsonb_text_bytes(&json!([])), 2);
        // A quote, a backslash and a newline are escaped; DEL and non-ASCII text are not.
        let text = "q\"\\\n\u{7f}\u{e9}";
        assert_eq!(
            jsonb_text_bytes(&json!(text)),
            1 + 1 + 2 + 2 + 2 + 1 + 2 + 1
        );
    }
}

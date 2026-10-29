//! Canonical JSON for digests that clients recompute: table summary receipts, the result kind registry and kind check
//! receipts.
use serde_json::Value;

/// RFC 8785 canonical JSON: object keys sorted by UTF-16 code units at every level, no whitespace, serde_json's string
/// escaping, which matches the standard's, and each number as ECMAScript writes the double it denotes: `1.0` is `1`,
/// `1e21` is `1e+21`, and an integer past 2^53 is the nearest double.
pub fn canonical_json(value: &Value) -> Vec<u8> {
    fn write(value: &Value, out: &mut Vec<u8>) {
        match value {
            Value::Object(map) => {
                let mut entries: Vec<(&String, &Value)> = map.iter().collect();
                entries.sort_by(|left, right| left.0.encode_utf16().cmp(right.0.encode_utf16()));
                out.push(b'{');
                for (at, (key, item)) in entries.into_iter().enumerate() {
                    if at > 0 {
                        out.push(b',');
                    }
                    out.extend(serde_json::to_vec(key).unwrap_or_default());
                    out.push(b':');
                    write(item, out);
                }
                out.push(b'}');
            }
            Value::Array(items) => {
                out.push(b'[');
                for (at, item) in items.iter().enumerate() {
                    if at > 0 {
                        out.push(b',');
                    }
                    write(item, out);
                }
                out.push(b']');
            }
            Value::Number(number) => out.extend(number_text(number).as_bytes()),
            scalar => out.extend(serde_json::to_vec(scalar).unwrap_or_default()),
        }
    }
    let mut out = Vec::new();
    write(value, &mut out);
    out
}

/// A number as ECMAScript's `Number.prototype.toString` writes the double it denotes (RFC 8785 section 3.2.2.3).
fn number_text(number: &serde_json::Number) -> String {
    const EXACT: u64 = 1 << 53;
    if let Some(n) = number.as_u64().filter(|n| *n <= EXACT) {
        return n.to_string();
    }
    if let Some(n) = number.as_i64().filter(|n| n.unsigned_abs() <= EXACT) {
        return n.to_string();
    }
    // A serde_json number is always finite.
    double_text(number.as_f64().unwrap_or_default())
}

fn double_text(value: f64) -> String {
    if value == 0.0 {
        return "0".into();
    }
    if value < 0.0 {
        return format!("-{}", double_text(-value));
    }
    // With the shortest form's k digits and its decimal point n places from their start, ECMAScript's layout is:
    let (digits, n) = shortest(value);
    let digits = digits.as_str();
    let k = digits.len() as i32;
    if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        let (whole, fraction) = digits.split_at(n as usize);
        format!("{whole}.{fraction}")
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat(n.unsigned_abs() as usize))
    } else {
        let sign = if n > 0 { '+' } else { '-' };
        let (first, rest) = digits.split_at(1);
        let point = if rest.is_empty() { "" } else { "." };
        format!("{first}{point}{rest}e{sign}{}", (n - 1).unsigned_abs())
    }
}

/// The digits of `d[.ddd]e<exponent>` without trailing zeros, and where the decimal point falls from their start.
fn digits_and_point(scientific: &str) -> (String, i32) {
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((scientific, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let point = exponent.parse::<i32>().unwrap_or_default() + 1;
    (digits.trim_end_matches('0').to_owned(), point)
}

/// The fewest digits that read back as `value`, as ECMAScript chooses them: of the forms that short, the one closest to
/// `value`, and of two equally close the one whose last digit is even. Rust's shortest form is as short and as close but
/// can take the odd digit of a tie, so the tie is settled here from the exact value.
fn shortest(value: f64) -> (String, i32) {
    let (digits, point) = digits_and_point(&format!("{value:e}"));
    let unit = point - digits.len() as i32;
    // A double's exact decimal expansion has at most 767 significant digits.
    let exact = format!("{value:.800e}");
    let (mantissa, exponent) = exact.split_once('e').unwrap_or((&exact, "0"));
    let exact_digits: Vec<u8> = mantissa.bytes().filter(u8::is_ascii_digit).collect();
    let exact_point = exponent.parse::<i32>().unwrap_or_default() + 1;
    // The exact digits down to the shortest form's last place make `below`; the rest is the remainder past it.
    let Ok(taken) = usize::try_from(exact_point - unit) else {
        return (digits, point);
    };
    if taken > 19 || taken > exact_digits.len() {
        return (digits, point);
    }
    let below = exact_digits[..taken]
        .iter()
        .fold(0_u64, |n, d| n * 10 + u64::from(d - b'0'));
    let rest = &exact_digits[taken..];
    let half = match rest.first().map(|d| d.cmp(&b'5')) {
        None => std::cmp::Ordering::Less,
        Some(std::cmp::Ordering::Equal) if rest[1..].iter().any(|d| *d != b'0') => {
            std::cmp::Ordering::Greater
        }
        Some(order) => order,
    };
    let closest = match half {
        std::cmp::Ordering::Less => below,
        std::cmp::Ordering::Greater => below + 1,
        std::cmp::Ordering::Equal if below % 2 == 0 => below,
        std::cmp::Ordering::Equal => below + 1,
    };
    let text = format!("{closest}e{unit}");
    if closest > 0 && text.parse::<f64>() == Ok(value) {
        let (closest_digits, _) = digits_and_point(&format!("{closest}e0"));
        let closest_point = closest.to_string().len() as i32 + unit;
        if closest_digits.len() == digits.len() {
            return (closest_digits, closest_point);
        }
    }
    (digits, point)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_json_sorts_keys_by_utf16_code_units() {
        // UTF-8 bytes put U+FF61 first; UTF-16 code units put U+1F600 (a surrogate pair) first, as RFC 8785 does.
        let value = json!({"\u{FF61}": 1, "\u{1F600}": [{"b": null, "a": "x\ny"}]});
        assert_eq!(
            String::from_utf8(canonical_json(&value)).unwrap(),
            "{\"\u{1F600}\":[{\"a\":\"x\\ny\",\"b\":null}],\"\u{FF61}\":1}"
        );
    }

    #[test]
    fn numbers_take_the_standards_form() {
        // RFC 8785 appendix B: the bits of a double and the text the standard requires for it.
        for (bits, text) in [
            (0x0000000000000000_u64, "0"),
            (0x8000000000000000, "0"),
            (0x0000000000000001, "5e-324"),
            (0x8000000000000001, "-5e-324"),
            (0x7fefffffffffffff, "1.7976931348623157e+308"),
            (0xffefffffffffffff, "-1.7976931348623157e+308"),
            (0x4340000000000000, "9007199254740992"),
            (0xc340000000000000, "-9007199254740992"),
            (0x4430000000000000, "295147905179352830000"),
            (0x44b52d02c7e14af5, "9.999999999999997e+22"),
            (0x44b52d02c7e14af6, "1e+23"),
            (0x44b52d02c7e14af7, "1.0000000000000001e+23"),
            (0x444b1ae4d6e2ef4e, "999999999999999700000"),
            (0x444b1ae4d6e2ef4f, "999999999999999900000"),
            (0x444b1ae4d6e2ef50, "1e+21"),
            (0x3eb0c6f7a0b5ed8c, "9.999999999999997e-7"),
            (0x3eb0c6f7a0b5ed8d, "0.000001"),
            (0x41b3de4355555553, "333333333.3333332"),
            (0x41b3de4355555554, "333333333.33333325"),
            (0x41b3de4355555555, "333333333.3333333"),
            (0x41b3de4355555556, "333333333.3333334"),
            (0x41b3de4355555557, "333333333.33333343"),
            (0xbecbf647612f3696, "-0.0000033333333333333333"),
            (0x43143ff3c1cb0959, "1424953923781206.2"),
        ] {
            let value = serde_json::Number::from_f64(f64::from_bits(bits)).unwrap();
            assert_eq!(number_text(&value), text, "{bits:016x}");
        }
        // The same double read from other texts takes one form; integers are exact up to 2^53, then doubles.
        for (read, text) in [
            ("1.0", "1"),
            ("1e0", "1"),
            ("-0", "0"),
            ("-0.0", "0"),
            ("100", "100"),
            ("1E21", "1e+21"),
            ("0.0000001", "1e-7"),
            ("123.4500", "123.45"),
            ("9007199254740993", "9007199254740992"),
            ("-9007199254740993", "-9007199254740992"),
            ("18446744073709551615", "18446744073709552000"),
        ] {
            let value: Value = serde_json::from_str(read).unwrap();
            assert_eq!(
                String::from_utf8(canonical_json(&value)).unwrap(),
                text,
                "{read}"
            );
        }
    }
}

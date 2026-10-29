//! Numbers, names and placeholders in table statements and summaries.
use super::*;

/// Too many findings, with the counts and limits the next lead call needs to correct it.
pub(super) fn findings_too_many(written: usize, composed: usize) -> CodeRefused {
    CodeRefused {
        code: "proposal_findings_too_many",
        names: vec![
            format!("written: {written}"),
            format!("from table statements: {composed}"),
            format!("at most {FINDINGS_MAX} written and {ALL_FINDINGS_MAX} in all"),
        ],
    }
}

/// What a masked number reads as in the summary; it holds no digit and no brace.
pub(super) const NUMBER_MASK: &str = "(a number)";

/// The summary's limit in bytes, as `Proposal::validate` checks it.
pub(super) const SUMMARY_MAX: usize = 2048;

/// The text with each number the prose rule refuses written as `NUMBER_MASK`, read with the same tokens as `numbers`,
/// and how many it replaced. A number inside a name stays, so the prose rule still refuses it.
pub(super) fn mask_refused_numbers(text: &str, index: &ResultIndex) -> (String, usize) {
    let chars: Vec<char> = text.chars().collect();
    let (mut out, mut masked, mut last) = (String::new(), 0, 0);
    for (start, end) in number_spans(&chars) {
        let number: String = chars[start..end].iter().collect();
        if index.allowed_numbers.contains(&number) || in_a_name(&chars, start, end) {
            continue;
        }
        out.extend(&chars[last..start]);
        out.push_str(NUMBER_MASK);
        masked += 1;
        last = end;
    }
    out.extend(&chars[last..]);
    (out, masked)
}

/// Whether the number at `start..end` belongs to a name such as B12, HbA1c, IL-6 or omega-3: a letter touches it, or
/// a hyphen or an invisible joiner (`joins_a_name`) joins it to a letter before. Masked, the name would read as
/// B(a number).
pub(super) fn in_a_name(chars: &[char], start: usize, end: usize) -> bool {
    let letter = |i: Option<usize>| {
        i.and_then(|i| chars.get(i))
            .is_some_and(|c| c.is_alphabetic())
    };
    letter(start.checked_sub(1))
        || letter(Some(end))
        || (start >= 2 && joins_a_name(chars[start - 1]) && letter(Some(start - 2)))
}

/// The token a number inside a name (`in_a_name`) stands in: the run of characters around it up to whitespace or a
/// separator prose puts between words (a comma, semicolon, bracket, quote, `!` or `?`), with `.`, `:`, `-`, `_` and `/`
/// trimmed from its ends. Every other character stays in the token, so two names joined by a slash, a colon, an
/// invisible character, a combining mark or a letter from another script read as one token that must itself be shown
/// (`plain_name`): `Fmo3`, `IL-6`, `C57BL/6J`.
pub(super) fn name_around(chars: &[char], start: usize, end: usize) -> String {
    raw_token(chars, start, end)
        .trim_matches(['.', ':', '-', '_', '/'])
        .to_owned()
}

/// The token around `start..end` exactly as written: every character up to whitespace or a separator prose puts between
/// words. `name_around` reads the same token and trims its ends.
pub(super) fn raw_token(chars: &[char], start: usize, end: usize) -> String {
    let stop = |c: char| {
        c.is_whitespace()
            || matches!(
                c,
                ',' | ';'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '"'
                    | '\''
                    | '!'
                    | '?'
                    | '`'
                    | '\u{2018}'
                    | '\u{2019}'
                    | '\u{201C}'
                    | '\u{201D}'
                    | '\u{2026}'
            )
    };
    let mut from = start;
    while from > 0 && !stop(chars[from - 1]) {
        from -= 1;
    }
    let mut to = end;
    while to < chars.len() && !stop(chars[to]) {
        to += 1;
    }
    chars[from..to].iter().collect()
}

/// How a step literal occurs in prose: the raw token with at most one sentence-final `.` or `:` after its last digit
/// removed. Nothing else is trimmed, so a sign, a leading point or any wrapper stays and the occurrence matches a
/// literal only when it writes that literal exactly (`7`, `-4`, `10.5`, `7.` at a sentence's end; never `.7`, `-.7`,
/// `_7_`, `/7/` or `-7` for `7`).
pub(super) fn literal_occurrence(chars: &[char], start: usize, end: usize) -> String {
    let token = raw_token(chars, start, end);
    match token.strip_suffix(['.', ':']) {
        Some(stem) if stem.ends_with(|c: char| c.is_ascii_digit()) => stem.to_owned(),
        _ => token,
    }
}

/// Whether a token holding a letter and a digit stands only where a source writes it whole: everything but a quantity
/// with a unit (`7.1mg`, `6J`: ASCII digits with at most one point, then ASCII letters) and an ASCII expression (`n=12`,
/// `p<0.05`), which keep the rule for numbers. So `1e-9` composed from an allowed 1 and 9, `Gene3` with a leading
/// invisible character and `A7/B3` are refused unless shown whole.
pub(super) fn needs_whole_token(token: &str) -> bool {
    if !token.chars().any(char::is_alphabetic) || !token.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    if token.is_ascii() && token.contains(['=', '<', '>', '%']) {
        return false;
    }
    let split = token
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(token.len());
    let (number, unit) = token.split_at(split);
    let unit_shaped = number.starts_with(|c: char| c.is_ascii_digit())
        && number.matches('.').count() <= 1
        && !unit.is_empty()
        && unit.chars().all(|c| c.is_ascii_alphabetic());
    !unit_shaped
}

/// A name a number may stand in when the call shows it: ASCII letters and digits with inner hyphens, periods,
/// underscores or slashes, starting with a letter and holding a digit (Fmo3, IL-6, rs12345, C57BL/6J). A token starting
/// with a digit is a quantity or a value (`7.1mg`, `1e-9`, `6J`), and one with any other character is not a plain name.
pub(super) fn plain_name(token: &str) -> bool {
    let bytes = token.as_bytes();
    token.is_ascii()
        && bytes.first().is_some_and(u8::is_ascii_alphabetic)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.iter().any(u8::is_ascii_digit)
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'/'))
}

/// Whether the call shows the name a number stands in (`name_around`): the whole token is a plain name its sources write,
/// or, in a token of ASCII letters, digits and visible separators (`visible_separator`) where exactly one segment between
/// the separators holds a digit, that segment is; names compare without regard to ASCII case. So `FMO3` stands where
/// the sources write `Fmo3`, and so do `Fmo3-dependent`, `Fmo3−/−` and `anti-Fmo3`. Two digit-bearing names joined into
/// one token (`A7/B3`) still read as one name nothing showed, a segment that starts with a digit (`1e-9`, `Fmo3.5`) is
/// never a name, a token with an invisible character, another script's letter or a digit outside ASCII never splits,
/// and every digit stays one the sources wrote. `allowed` holds `shown_name_keys`
/// of the sources.
pub(super) fn shown_name(name: &str, allowed: &HashSet<String>) -> bool {
    if plain_name(name) && allowed.contains(&name.to_ascii_lowercase()) {
        return true;
    }
    digit_segments(name).is_some_and(|segments| {
        matches!(segments.as_slice(), [one] if plain_name(one) && allowed.contains(&one.to_ascii_lowercase()))
    })
}

/// A separator a reader sees between two parts of a name: an ASCII hyphen, slash, period or underscore, or a visible
/// Unicode hyphen, dash or minus sign. Invisible joiners and the soft hyphen are not among them.
fn visible_separator(c: char) -> bool {
    matches!(
        c,
        '-' | '/' | '.' | '_' | '\u{2010}'
            ..='\u{2015}' | '\u{2212}' | '\u{FE58}' | '\u{FE63}' | '\u{FF0D}'
    )
}

/// The segments of `name` between visible separators that hold a digit, when `name` holds nothing but ASCII letters,
/// digits and visible separators; `None` otherwise.
fn digit_segments(name: &str) -> Option<Vec<&str>> {
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || visible_separator(c))
        .then(|| {
            name.split(visible_separator)
                .filter(|s| s.bytes().any(|b| b.is_ascii_digit()))
                .collect()
        })
}

/// The keys under which `text`'s names holding a number are shown (`shown_name`): each such token that is a plain
/// name, and each of its digit-bearing segments that is one, in ASCII lower case.
pub(super) fn shown_name_keys(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut keys = Vec::new();
    for (start, end) in number_spans(&chars) {
        if !in_a_name(&chars, start, end) {
            continue;
        }
        let name = name_around(&chars, start, end);
        // A name, or any other ASCII token with a letter written whole (`1e-9`), is shown as written.
        if plain_name(&name) || (name.is_ascii() && needs_whole_token(&name)) {
            keys.push(name.to_ascii_lowercase());
        }
        // A shorter key only for a token whose one digit-bearing segment it is: `Fmo3-dependent` shows `Fmo3`, while
        // `Fmo3.5` shows only itself, so its `3` never stands alone in a name.
        if let Some([segment]) = digit_segments(&name).as_deref()
            && *segment != name
            && plain_name(segment)
        {
            keys.push(segment.to_ascii_lowercase());
        }
    }
    keys
}

/// A hyphen in any of its Unicode forms (IL-6 written with a nonbreaking hyphen, an en dash or a minus sign), a soft
/// hyphen or a zero-width joiner, which a name may hold between its letters and its number.
pub(super) fn joins_a_name(c: char) -> bool {
    matches!(
        c,
        '-' | '\u{00AD}'
            | '\u{2010}'..='\u{2015}'
            | '\u{200B}'..='\u{200D}'
            | '\u{2060}'
            | '\u{2212}'
            | '\u{FE58}'
            | '\u{FE63}'
            | '\u{FF0D}'
    )
}

/// At most this many field-and-number names reach the next call after a number refusal.
pub(super) const REFUSED_NUMBERS_MAX: usize = 8;

/// Each field and number that broke the prose rule, as `field: number`, once each, at most eight. They go to the next
/// model call only: a number in prose may be a result value, so logs and run events keep the fixed code alone.
pub fn refused_numbers<'a>(
    index: &ResultIndex,
    texts: impl IntoIterator<Item = (&'static str, &'a str)>,
) -> Vec<String> {
    refused_numbers_with(
        index,
        texts.into_iter().map(|(field, text)| (field, text, None)),
    )
}

/// `refused_numbers` where a text may also hold the numbers in its own allowance: a step's title and rationale take the
/// numbers that step's own SQL sets (`step_literals::step_own_numbers`) where they stand alone
/// (`ResultIndex::step_prose_refused_own`), and no other text takes any.
pub(super) fn refused_numbers_with<'a>(
    index: &ResultIndex,
    texts: impl IntoIterator<Item = (&'static str, &'a str, Option<&'a HashSet<String>>)>,
) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for (field, text, own) in texts {
        let refused = match (
            own,
            matches!(field, "actions.title" | "actions.rationale" | "reason"),
        ) {
            (Some(own), true) => index.step_prose_refused_own(text, own),
            (None, true) => index.step_prose_refused(text),
            (_, false) => index.prose_refused(text),
        };
        for number in refused {
            let name = format!("{field}: {}", number.chars().take(24).collect::<String>());
            if !names.contains(&name) && names.len() < REFUSED_NUMBERS_MAX {
                names.push(name);
            }
        }
    }
    names
}

/// Numbers written as digits, with inner decimal points or thousands separators.
pub(super) fn numbers(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    number_spans(&chars)
        .into_iter()
        .map(|(start, end)| chars[start..end].iter().collect())
        .collect()
}

/// Where each number starts and ends, in characters: a run of digits with inner decimal points or thousands separators.
pub(super) fn number_spans(chars: &[char]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_digit()
                    || (matches!(chars[i], '.' | ',')
                        && chars.get(i + 1).is_some_and(|c| c.is_ascii_digit())))
            {
                i += 1;
            }
            out.push((start, i));
        } else {
            i += 1;
        }
    }
    out
}

/// A placeholder is a short lowercase name in braces, such as `{a}` or `{oj_mean}`.
pub(super) fn placeholder_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 24
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// The placeholder names in a statement, or None when a brace is not part of a well-formed placeholder.
pub(super) fn placeholders(statement: &str) -> Option<BTreeSet<String>> {
    let mut used = BTreeSet::new();
    let mut rest = statement;
    while let Some(at) = rest.find(['{', '}']) {
        if rest[at..].starts_with('}') {
            return None;
        }
        let after = &rest[at + 1..];
        let close = after
            .find(['{', '}'])
            .filter(|&i| after[i..].starts_with('}'))?;
        if !placeholder_name(&after[..close]) {
            return None;
        }
        used.insert(after[..close].to_owned());
        rest = &after[close + 1..];
    }
    Some(used)
}

/// A column's name as a composed finding writes it: underscores and hyphens read as spaces, and braces as parentheses,
/// since a brace in a sentence marks a value. Digits stay as they were, so the prose rule reads the same numbers the
/// shown column name allowed.
pub(super) fn column_words(column: &str) -> String {
    let words = column
        .replace(['_', '-'], " ")
        .replace('{', "(")
        .replace('}', ")")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if words.is_empty() {
        column.to_owned()
    } else {
        words
    }
}

/// The statement with each placeholder replaced by a space, so a digit in a name is not read as a number.
pub(super) fn without_placeholders(statement: &str) -> String {
    let mut out = String::new();
    let mut rest = statement;
    while let Some(at) = rest.find('{') {
        out.push_str(&rest[..at]);
        match rest[at + 1..].find('}') {
            Some(close) => {
                out.push(' ');
                rest = &rest[at + 1 + close + 1..];
            }
            None => {
                out.push_str(&rest[at..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

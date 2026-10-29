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
/// and its direction controls dropped (`without_directions`), with how many it replaced or dropped. A number inside a
/// name or a token that must be shown whole stays (`may_mask`), so the prose rule still refuses it.
pub(super) fn mask_refused_numbers(text: &str, index: &ResultIndex) -> (String, usize) {
    let chars: Vec<char> = text.chars().collect();
    let (mut out, mut masked, mut last) = (String::new(), 0, 0);
    for (start, end) in number_spans(&chars) {
        if !may_mask(&chars, start, end) || !index.number_refused(&chars, start, end, false, None) {
            continue;
        }
        out.extend(&chars[last..start]);
        out.push_str(NUMBER_MASK);
        masked += 1;
        last = end;
    }
    out.extend(&chars[last..]);
    let (out, dropped) = without_directions(out);
    (out, masked + dropped)
}

/// `text` without its direction controls (`directs_text`), and how many it dropped. Both masking paths drop them: no
/// number, and no bound value a document inserts later, may display in an order they set.
pub(super) fn without_directions(text: String) -> (String, usize) {
    let dropped = text.chars().filter(|&c| directs_text(c)).count();
    if dropped == 0 {
        return (text, 0);
    }
    (
        text.chars().filter(|&c| !directs_text(c)).collect(),
        dropped,
    )
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

/// The token around `start..end` exactly as written: every character up to whitespace or a separator prose puts between
/// words. `operand_around` cuts the same token at comparisons and trims its ends.
pub(super) fn raw_token(chars: &[char], start: usize, end: usize) -> String {
    let (from, to) = run_bounds(chars, start, end, separates_words);
    chars[from..to].iter().collect()
}

/// The run around `start..end` up to a character `stop` names, except inside a digit grouping (`groups_digits`).
fn run_bounds(
    chars: &[char],
    start: usize,
    end: usize,
    stop: impl Fn(char) -> bool,
) -> (usize, usize) {
    let stops = |i: usize| stop(chars[i]) && !groups_digits(chars, i);
    let mut from = start;
    while from > 0 && !stops(from - 1) {
        from -= 1;
    }
    let mut to = end;
    while to < chars.len() && !stops(to) {
        to += 1;
    }
    (from, to)
}

/// Whether `chars[i]` lies in a run of spaces, quote marks and hidden characters (`hides_digits`) between two digits.
/// Such a run groups the digits into one number however many characters it has and whichever they are: thousands are
/// grouped with a space (`1 234`), a narrow or thin space (`1\u{202F}234`) and in some locales an apostrophe (`1'234`,
/// `1\u{2019}234`), and a hidden character between spaces (`1 \u{200B} 234`) shows as their grouping. A comma, a
/// semicolon, a bracket or a word between two numbers separates them.
fn groups_digits(chars: &[char], i: usize) -> bool {
    let quote = |c: char| {
        matches!(
            c,
            '\'' | '"' | '`' | '\u{2018}' | '\u{2019}' | '\u{201C}' | '\u{201D}'
        )
    };
    let in_run = |c: char| c.is_whitespace() || quote(c) || hides_digits(c);
    if !in_run(chars[i]) {
        return false;
    }
    let mut from = i;
    while from > 0 && in_run(chars[from - 1]) {
        from -= 1;
    }
    let mut to = i + 1;
    while to < chars.len() && in_run(chars[to]) {
        to += 1;
    }
    from > 0
        && chars[from - 1].is_ascii_digit()
        && chars.get(to).is_some_and(|c| c.is_ascii_digit())
}

/// A separator prose puts between words: whitespace, a comma, semicolon, bracket, quote, `!`, `?` or an ellipsis.
fn separates_words(c: char) -> bool {
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
}

/// A comparison between two operands (`n=12`, `p<0.05`, `r≥0.8`), which cuts a token into operands.
fn compares(c: char) -> bool {
    matches!(
        c,
        '=' | '<' | '>' | '\u{2248}' | '\u{2260}' | '\u{2264}' | '\u{2265}'
    )
}

/// The operand a number stands in: its token (`raw_token`) cut at comparisons (`compares`). `.`, `:`, `-`, `_` and `/`
/// are trimmed from its end, and from its start only before a letter (`-Fmo3`), since a sign or a point before a digit
/// makes another value (`-1e-9`, `.1e-9`). In `p=1e-9` the 1 and the 9 stand in `1e-9`, and in `<Gene3>` the 3 stands
/// in `Gene3`. Every other character stays, so two names joined by a slash, a colon, an invisible character, a
/// combining mark or a letter from another script read as one operand that must itself be shown: `Fmo3`, `IL-6`,
/// `C57BL/6J`.
pub(super) fn operand_around(chars: &[char], start: usize, end: usize) -> String {
    let (from, to) = run_bounds(chars, start, end, |c| separates_words(c) || compares(c));
    let run: String = chars[from..to].iter().collect();
    let run = run.trim_end_matches(['.', ':', '-', '_', '/']);
    let lettered = run.trim_start_matches(['.', ':', '-', '_', '/']);
    if lettered.starts_with(char::is_alphabetic) {
        lettered.to_owned()
    } else {
        run.to_owned()
    }
}

/// Whether a character sets the direction text is displayed in: an embedding, override or isolate, its closing mark,
/// or a directional mark. Its scope crosses spaces and tokens (`\u{202E} 37 \u{202C}` displays as 73), so a text holding
/// one has every number refused (`number_refused`).
pub(super) fn directs_text(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

/// Whether a token holds a character that hides or changes how the digits around it read: a control character, an
/// invisible or format character (a zero-width space or joiner, a direction mark, a variation selector, a filler), a
/// combining mark, or any other non-ASCII character that is neither a letter nor a symbol written with numbers (`°`,
/// `±`, `×`, a comparison, a Unicode hyphen or minus). Every number in such a token is refused: `1\u{200B}9` reads as 19.
pub(super) fn obscures_numbers(token: &str) -> bool {
    token.chars().any(hides_digits)
}

/// One character `obscures_numbers` refuses a token for.
fn hides_digits(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{115F}' | '\u{1160}' | '\u{17B4}' | '\u{17B5}' | '\u{3164}' | '\u{FFA0}'
        )
        || (!c.is_ascii()
            && !c.is_alphabetic()
            && !visible_separator(c)
            && !compares(c)
            && !matches!(
                c,
                '\u{00B0}'
                    | '\u{00B1}'
                    | '\u{00B7}'
                    | '\u{00D7}'
                    | '\u{2030}'
                    | '\u{2032}'
                    | '\u{2033}'
                    | '\u{2192}'
            ))
}

/// Whether a refused number may be written as `NUMBER_MASK`. In an operand (`operand_around`) that starts with a digit
/// every refused number masks, which leaves no name behind (`(a number)mg`, `(a number)FFh`, `(a number)e-(a number)`).
/// Otherwise the number masks only outside a name (`in_a_name`) in an operand with no letter: masking the `1` of
/// `Fmo3-1` would leave a shown `Fmo3` standing for a name nothing showed, while the `12` of `n=12` and both numbers of
/// `10^-9` mask cleanly.
pub(super) fn may_mask(chars: &[char], start: usize, end: usize) -> bool {
    let operand = operand_around(chars, start, end);
    operand.starts_with(|c: char| c.is_ascii_digit())
        || (!in_a_name(chars, start, end) && !operand.chars().any(char::is_alphabetic))
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

/// Whether an operand (`operand_around`) stands only where a source writes it whole: one holding two or more numbers
/// (`10^-9`, `10/20`, `1\u{202F}234`, `1'234`), and one holding a number and a letter unless it is a measurement with a
/// listed unit (`unit_shaped`). So `1e-9` composed from an allowed 1 and 9, `Gene3%`, `<Gene3>`, `A7/B3`, `0xFF`,
/// `0FFh` and `5k` are refused unless a source writes them whole.
pub(super) fn needs_whole_token(token: &str) -> bool {
    let chars: Vec<char> = token.chars().collect();
    match number_spans(&chars).len() {
        0 => false,
        1 => chars.iter().any(|c| c.is_alphabetic()) && !unit_shaped(token),
        _ => true,
    }
}

/// Units a measurement may carry with its number in prose: mass, volume, amount and concentration, time, temperature,
/// length, sequence and molecular size, energy, speed, fold and ordinal suffixes. None of them changes the number's
/// own value, so a listed unit keeps the per-number rule. A multiplier (`5k`, `5M`, `5B`) or a radix (`0xFF`, `0FFh`)
/// writes another value and is not listed; nor is bare `M`, which reads as molar or as a million.
#[rustfmt::skip]
const UNITS: &[&str] = &[
    "g", "kg", "mg", "\u{00B5}g", "\u{03BC}g", "ug", "ng", "pg", "fg", "L", "l", "mL", "ml", "dL", "dl", "\u{00B5}L",
    "\u{03BC}L", "uL", "nL", "mol", "mmol", "\u{00B5}mol", "\u{03BC}mol", "umol", "nmol", "pmol", "mM", "\u{00B5}M",
    "\u{03BC}M", "uM", "nM", "pM", "fM", "ppm", "ppb", "IU", "U", "mU", "s", "sec", "ms", "min", "h", "hr", "hrs",
    "d", "wk", "wks", "mo", "y", "yr", "yrs", "\u{00B0}C", "\u{00B0}F", "C", "m", "cm", "mm", "\u{00B5}m",
    "\u{03BC}m", "um", "nm", "\u{00C5}", "bp", "kb", "kbp", "Mb", "Mbp", "Gb", "cM", "nt", "aa", "Da", "kDa", "cal",
    "kcal", "Cal", "J", "kJ", "rpm", "x", "xg", "st", "nd", "rd", "th",
];

/// A measurement with a listed unit: digits with at most one point, then units from `UNITS` joined by `/` (`7.1mg`,
/// `6J`, `37\u{00B0}C`, `5\u{00B5}g`, `5mg/kg`, `12wk`, `3rd`).
pub(super) fn unit_shaped(token: &str) -> bool {
    let split = token
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(token.len());
    let (number, unit) = token.split_at(split);
    number.starts_with(|c: char| c.is_ascii_digit())
        && number.matches('.').count() <= 1
        && !unit.is_empty()
        && unit.split('/').all(|part| UNITS.contains(&part))
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

/// Whether the call shows the name a number stands in (`operand_around`): the whole token is a plain name its sources write,
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
        let name = operand_around(&chars, start, end);
        // A name, or any other ASCII operand that must be shown whole (`1e-9`), is shown as written.
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

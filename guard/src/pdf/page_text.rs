//! The text interpreter: shown text, page boxes and hidden text.
use super::*;

#[derive(Clone, Copy, Debug)]
pub(super) struct State {
    pub(super) ctm: Matrix,
    /// None when the fill colour is not a plain device colour (a pattern or a separation).
    pub(super) fill_white: Option<bool>,
    pub(super) render: i64,
    pub(super) font_size: f64,
    pub(super) bytes_per_char: u64,
    pub(super) leading: f64,
}

pub(super) struct Interpreter<'a> {
    pub(super) document: &'a Document,
    pub(super) box_: [f64; 4],
    pub(super) counts: Hidden,
    pub(super) operations: u64,
    pub(super) content_bytes: usize,
    pub(super) forms: HashSet<ObjectId>,
}

pub(super) fn number(object: &Object) -> Option<f64> {
    match object {
        Object::Integer(i) => Some(*i as f64),
        Object::Real(r) => Some(f64::from(*r)),
        _ => None,
    }
}

pub(super) fn numbers(operands: &[Object]) -> Option<Vec<f64>> {
    operands.iter().map(number).collect()
}

pub(super) fn white(values: &[f64]) -> Option<bool> {
    match values.len() {
        1 => Some(values[0] >= 0.999),
        3 => Some(values.iter().all(|v| *v >= 0.999)),
        4 => Some(values.iter().all(|v| *v <= 0.001)),
        _ => None,
    }
}

pub(super) fn shown_bytes(operands: &[Object]) -> u64 {
    operands
        .iter()
        .map(|o| match o {
            Object::String(bytes, _) => bytes.len() as u64,
            Object::Array(items) => items
                .iter()
                .map(|i| {
                    if let Object::String(b, _) = i {
                        b.len() as u64
                    } else {
                        0
                    }
                })
                .sum(),
            _ => 0,
        })
        .sum()
}

impl Interpreter<'_> {
    pub(super) fn font_bytes_per_char(&self, resources: Option<&Dictionary>, font: &[u8]) -> u64 {
        let font = resources
            .and_then(|r| r.get(b"Font").ok())
            .and_then(|f| dict_of(self.document, f))
            .and_then(|fonts| fonts.get(font).ok())
            .and_then(|f| dict_of(self.document, f));
        if font.and_then(|f| name(f, b"Subtype")) == Some(b"Type0") {
            2
        } else {
            1
        }
    }

    pub(super) fn show(&mut self, state: &State, tm: Matrix, bytes: u64) {
        let chars = bytes / state.bytes_per_char.max(1);
        if chars == 0 {
            return;
        }
        let trm = tm.then(state.ctm);
        let size = state.font_size.abs() * trm.scale();
        let [x, y] = [trm.0[4], trm.0[5]];
        let [x0, y0, x1, y1] = self.box_;
        if matches!(state.render, 3 | 7) {
            self.counts.invisible += chars;
        } else if state.fill_white == Some(true) && matches!(state.render, 0 | 2 | 4 | 6) {
            self.counts.white += chars;
        }
        if size < 1.0 {
            self.counts.tiny += chars;
        }
        if x < x0 - PAGE_MARGIN
            || x > x1 + PAGE_MARGIN
            || y < y0 - PAGE_MARGIN
            || y > y1 + PAGE_MARGIN
        {
            self.counts.offpage += chars;
        }
    }

    pub(super) fn run(
        &mut self,
        bytes: &[u8],
        resources: Option<&Dictionary>,
        start: State,
        depth: usize,
    ) {
        let mut budget = OPERATION_BUDGET.saturating_sub(self.operations);
        let Ok(parsed) = content::parse(bytes, &mut budget) else {
            self.counts.truncated = true;
            return;
        };
        let mut stack: Vec<State> = Vec::new();
        let mut state = start;
        let mut tm = Matrix::IDENTITY;
        let mut tlm = Matrix::IDENTITY;
        for item in &parsed.items {
            let Item::Op(operation) = item else {
                continue;
            };
            self.operations += 1;
            let operands: Vec<Object> = operation.operands.iter().map(Operand::to_object).collect();
            let operands = &operands;
            match operation.operator {
                "q" if stack.len() < 64 => stack.push(state),
                "Q" => state = stack.pop().unwrap_or(state),
                "cm" => {
                    if let Some(m) = numbers(operands).filter(|m| m.len() == 6) {
                        state.ctm = Matrix([m[0], m[1], m[2], m[3], m[4], m[5]]).then(state.ctm);
                    }
                }
                "g" | "rg" | "k" | "sc" | "scn" => {
                    state.fill_white = numbers(operands).and_then(|v| white(&v));
                }
                "cs" => state.fill_white = Some(false),
                "BT" => {
                    tm = Matrix::IDENTITY;
                    tlm = Matrix::IDENTITY;
                }
                "Tf" => {
                    if let (Some(font), Some(size)) = (
                        operands.first().and_then(|o| o.as_name().ok()),
                        operands.get(1).and_then(number),
                    ) {
                        state.font_size = size;
                        state.bytes_per_char = self.font_bytes_per_char(resources, font);
                    }
                }
                "Tr" => state.render = operands.first().and_then(|o| o.as_i64().ok()).unwrap_or(0),
                "TL" => state.leading = operands.first().and_then(number).unwrap_or(state.leading),
                "Td" | "TD" => {
                    if let Some(v) = numbers(operands).filter(|v| v.len() == 2) {
                        if operation.operator == "TD" {
                            state.leading = -v[1];
                        }
                        tlm = Matrix([1.0, 0.0, 0.0, 1.0, v[0], v[1]]).then(tlm);
                        tm = tlm;
                    }
                }
                "Tm" => {
                    if let Some(m) = numbers(operands).filter(|m| m.len() == 6) {
                        tlm = Matrix([m[0], m[1], m[2], m[3], m[4], m[5]]);
                        tm = tlm;
                    }
                }
                "T*" => {
                    tlm = Matrix([1.0, 0.0, 0.0, 1.0, 0.0, -state.leading]).then(tlm);
                    tm = tlm;
                }
                "Tj" | "TJ" => self.show(&state, tm, shown_bytes(operands)),
                "'" | "\"" => {
                    tlm = Matrix([1.0, 0.0, 0.0, 1.0, 0.0, -state.leading]).then(tlm);
                    tm = tlm;
                    self.show(&state, tm, shown_bytes(operands));
                }
                "Do" if depth < FORM_DEPTH => {
                    let Some(form_name) = operands.first().and_then(|o| o.as_name().ok()) else {
                        continue;
                    };
                    let reference = resources
                        .and_then(|r| r.get(b"XObject").ok())
                        .and_then(|x| dict_of(self.document, x))
                        .and_then(|x| x.get(form_name).ok())
                        .and_then(|o| o.as_reference().ok());
                    let Some(id) = reference else { continue };
                    if !self.forms.insert(id) {
                        continue;
                    }
                    let Ok(Object::Stream(stream)) = self.document.get_object(id) else {
                        continue;
                    };
                    if name(&stream.dict, b"Subtype") != Some(b"Form") {
                        continue;
                    }
                    let remaining = CONTENT_BUDGET.saturating_sub(self.content_bytes);
                    let Ok(bytes) = stream.decompressed_content_with_limit(remaining) else {
                        self.counts.truncated = true;
                        continue;
                    };
                    self.content_bytes += bytes.len();
                    let matrix = stream
                        .dict
                        .get(b"Matrix")
                        .ok()
                        .and_then(|m| m.as_array().ok())
                        .and_then(|m| numbers(m))
                        .filter(|m| m.len() == 6)
                        .map(|m| Matrix([m[0], m[1], m[2], m[3], m[4], m[5]]))
                        .unwrap_or(Matrix::IDENTITY);
                    let form_resources = stream
                        .dict
                        .get(b"Resources")
                        .ok()
                        .and_then(|r| dict_of(self.document, r))
                        .or(resources);
                    let inner = State {
                        ctm: matrix.then(state.ctm),
                        ..state
                    };
                    self.run(&bytes, form_resources, inner, depth + 1);
                    self.forms.remove(&id);
                }
                // A form nested deeper than the count follows: the count is partial.
                "Do" => self.counts.truncated = true,
                _ => {}
            }
        }
    }
}

/// A page attribute from the page or the nearest page-tree node above it that sets it. Err when the page sits deeper
/// below the tree's root than the count follows, so the attribute may be one it did not reach.
pub(super) fn inherited<'a>(
    document: &'a Document,
    page: &'a Dictionary,
    key: &[u8],
) -> Result<Option<&'a Object>, TooDeep> {
    let mut current = page;
    for _ in 0..WALK_DEPTH {
        if let Ok(value) = current.get(key) {
            return Ok(deref(document, value));
        }
        let Some(parent) = current
            .get(b"Parent")
            .ok()
            .and_then(|p| dict_of(document, p))
        else {
            return Ok(None);
        };
        current = parent;
    }
    Err(TooDeep)
}

/// A page whose tree runs deeper than the count follows.
pub(super) struct TooDeep;

/// The page's crop box, else its media box, else US Letter. Err when either box may sit deeper than the count follows.
pub(super) fn page_box(document: &Document, page: &Dictionary) -> Result<[f64; 4], TooDeep> {
    let read = |key: &[u8]| -> Result<Option<Vec<f64>>, TooDeep> {
        Ok(inherited(document, page, key)?
            .and_then(|b| b.as_array().ok())
            .and_then(|b| numbers(b))
            .filter(|b| b.len() == 4))
    };
    let b = match read(b"CropBox")? {
        Some(b) => b,
        None => read(b"MediaBox")?.unwrap_or_else(|| vec![0.0, 0.0, 612.0, 792.0]),
    };
    Ok([
        b[0].min(b[2]),
        b[1].min(b[3]),
        b[0].max(b[2]),
        b[1].max(b[3]),
    ])
}

pub fn hidden_text(document: &Document) -> Hidden {
    let mut interpreter = Interpreter {
        document,
        box_: [0.0; 4],
        counts: Hidden::default(),
        operations: 0,
        content_bytes: 0,
        forms: HashSet::new(),
    };
    let pages = document.get_pages();
    if pages.len() > PAGE_LIMIT {
        interpreter.counts.truncated = true;
    }
    for (_, page_id) in pages.into_iter().take(PAGE_LIMIT) {
        let Ok(page) = document.get_dictionary(page_id) else {
            continue;
        };
        let (Ok(box_), Ok(resources)) = (
            page_box(document, page),
            inherited(document, page, b"Resources"),
        ) else {
            interpreter.counts.truncated = true;
            break;
        };
        interpreter.box_ = box_;
        let remaining = CONTENT_BUDGET.saturating_sub(interpreter.content_bytes);
        let Ok(content) = document.get_page_content_with_limit(page_id, remaining) else {
            interpreter.counts.truncated = true;
            break;
        };
        interpreter.content_bytes += content.len();
        let resources = resources.and_then(|r| match r {
            Object::Dictionary(d) => Some(d),
            _ => None,
        });
        let start = State {
            ctm: Matrix::IDENTITY,
            fill_white: Some(false),
            render: 0,
            font_size: 12.0,
            bytes_per_char: 1,
            leading: 0.0,
        };
        interpreter.run(&content, resources, start, 0);
        if interpreter.counts.truncated {
            break;
        }
    }
    interpreter.counts
}

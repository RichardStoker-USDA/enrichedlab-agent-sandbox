//! Walking page content and resources with their scopes.
use super::*;

// Stream use: a walk from the pages and the catalog through what each drawn content stream names.

/// Where a content stream's resource names resolve.
#[derive(Clone, Copy)]
pub(super) struct ResourceScope<'a> {
    pub(super) resources: Option<&'a Dictionary>,
    /// The resource dictionary's own object, or the object it is written inside: two scopes with one key are one.
    pub(super) key: Option<ObjectId>,
}

impl ResourceScope<'_> {
    const NONE: ResourceScope<'static> = ResourceScope {
        resources: None,
        key: None,
    };
}

/// What the walk found.
pub struct Uses {
    /// Every stream object's use.
    pub roles: BTreeMap<ObjectId, Role>,
    /// Images drawn inline in content, as image streams.
    pub inline: Vec<Stream>,
    /// Every drawn content stream was read within the budgets.
    pub complete: bool,
    /// Content whose bytes the lexer could not read: an inline image without an end.
    pub unreadable: u64,
    /// Operators, operands and delimiters the lexer left out.
    pub dropped: u64,
    /// Every string and name in the operands of the content read, once per content stream, for the scan.
    pub content_strings: Vec<Vec<u8>>,
    /// The content strings passed the strings budget; the scan counts the rest unscanned.
    pub content_strings_truncated: bool,
}

/// Calls `visit` with every string and name in an operand: dictionary keys and values, array items, nested.
pub(super) fn operand_strings(operand: &Operand, visit: &mut dyn FnMut(&[u8])) {
    match operand {
        Operand::String(bytes) | Operand::Name(bytes) => visit(bytes),
        Operand::Array(items) => {
            for item in items {
                operand_strings(item, visit);
            }
        }
        Operand::Dict(entries) => {
            for (key, value) in entries {
                visit(key);
                operand_strings(value, visit);
            }
        }
        Operand::Number(_) | Operand::Bool(_) | Operand::Null => {}
    }
}

pub(super) struct Walker<'a> {
    pub(super) document: &'a Document,
    /// Every drawing use the walk reached each stream by; `settle` turns the set into one role.
    pub(super) roles: HashMap<ObjectId, BTreeSet<Role>>,
    pub(super) entered: HashSet<ObjectId>,
    pub(super) steps: usize,
    /// Content to read: its streams (a page's may be several) and the scope its names resolve in.
    pub(super) queue: VecDeque<(Vec<ObjectId>, ResourceScope<'a>)>,
    pub(super) queued: HashSet<(Vec<ObjectId>, Option<ObjectId>)>,
    /// Operations left in the document's budget.
    pub(super) operations: u64,
    pub(super) content_bytes: usize,
    pub(super) complete: bool,
    pub(super) unreadable: u64,
    pub(super) dropped: u64,
    pub(super) inline: Vec<Stream>,
    /// Content whose operand strings were collected, so a stream read in several scopes is collected once.
    pub(super) collected: HashSet<Vec<ObjectId>>,
    pub(super) content_strings: Vec<Vec<u8>>,
    pub(super) content_string_bytes: usize,
    pub(super) content_strings_truncated: bool,
}

impl<'a> Walker<'a> {
    pub(super) fn new(document: &'a Document) -> Self {
        Walker {
            document,
            roles: HashMap::new(),
            entered: HashSet::new(),
            steps: 0,
            queue: VecDeque::new(),
            queued: HashSet::new(),
            operations: content::OPERATION_LIMIT,
            content_bytes: 0,
            complete: true,
            unreadable: 0,
            dropped: 0,
            inline: Vec::new(),
            collected: HashSet::new(),
            content_strings: Vec::new(),
            content_string_bytes: 0,
            content_strings_truncated: false,
        }
    }

    /// Keeps the strings and names in a parsed stream's operands and inline image dictionaries, within the budget.
    pub(super) fn collect_strings(&mut self, ids: &[ObjectId], items: &[Item]) {
        if !self.collected.insert(ids.to_vec()) {
            return;
        }
        let mut found = Vec::new();
        let mut keep = |bytes: &[u8]| found.push(bytes.to_vec());
        for item in items {
            match item {
                Item::Op(operation) => {
                    for operand in &operation.operands {
                        operand_strings(operand, &mut keep);
                    }
                }
                Item::Image(image) => {
                    for (key, value) in &image.entries {
                        keep(key);
                        operand_strings(value, &mut keep);
                    }
                }
            }
        }
        for bytes in found {
            if self.content_string_bytes + bytes.len() > STRINGS_LIMIT {
                self.content_strings_truncated = true;
                return;
            }
            self.content_string_bytes += bytes.len();
            self.content_strings.push(bytes);
        }
    }

    pub(super) fn get(&self, object: &'a Object) -> Option<(Option<ObjectId>, &'a Object)> {
        self.document.dereference(object).ok()
    }

    pub(super) fn dict(&self, object: &'a Object) -> Option<(Option<ObjectId>, &'a Dictionary)> {
        match self.get(object)? {
            (id, Object::Dictionary(d)) => Some((id, d)),
            (id, Object::Stream(s)) => Some((id, &s.dict)),
            _ => None,
        }
    }

    pub(super) fn stream(&self, object: &'a Object) -> Option<(Option<ObjectId>, &'a Stream)> {
        match self.get(object)? {
            (id, Object::Stream(s)) => Some((id, s)),
            _ => None,
        }
    }

    /// True the first time an object is entered, while the step budget lasts; past the budget the walk is incomplete.
    pub(super) fn enter(&mut self, id: Option<ObjectId>) -> bool {
        self.steps += 1;
        if self.steps > WALK_STEPS {
            self.complete = false;
            return false;
        }
        id.is_none_or(|id| self.entered.insert(id))
    }

    /// Records a use each time a stream is reached, before the walk decides whether to enter it again.
    pub(super) fn mark(&mut self, id: Option<ObjectId>, role: Role) {
        if let Some(id) = id {
            self.roles.entry(id).or_default().insert(role);
        }
    }

    pub(super) fn values(&self, object: &'a Object) -> Vec<&'a Object> {
        match self.get(object) {
            Some((_, Object::Dictionary(d))) => d.iter().map(|(_, v)| v).collect(),
            Some((_, Object::Array(items))) => items.iter().collect(),
            _ => Vec::new(),
        }
    }

    pub(super) fn items(&self, object: &'a Object) -> &'a [Object] {
        match self.get(object) {
            Some((_, Object::Array(items))) => items,
            _ => &[],
        }
    }

    /// A stream with no further keys to follow.
    pub(super) fn leaf(&mut self, object: &'a Object, role: Role) {
        if let Some((id, _)) = self.stream(object) {
            self.mark(id, role);
            self.enter(id);
        }
    }

    /// The scope a `Resources` entry gives, keyed by the dictionary's own object or by `owner` when written inline.
    pub(super) fn scope(
        &self,
        resources: &'a Object,
        owner: Option<ObjectId>,
    ) -> ResourceScope<'a> {
        match self.get(resources) {
            Some((id, Object::Dictionary(d))) => ResourceScope {
                resources: Some(d),
                key: id.or(owner),
            },
            _ => ResourceScope {
                resources: None,
                key: owner,
            },
        }
    }

    /// Queues content streams to read in a scope, once per scope.
    pub(super) fn queue_content(&mut self, ids: Vec<ObjectId>, scope: ResourceScope<'a>) {
        for id in &ids {
            self.mark(Some(*id), Role::Content);
        }
        if self.queued.insert((ids.clone(), scope.key)) {
            if self.queued.len() > SCOPE_LIMIT {
                self.complete = false;
                return;
            }
            self.queue.push_back((ids, scope));
        }
    }

    pub(super) fn document(&mut self) {
        for (_, page) in self.document.get_pages() {
            self.page(page);
        }
        if let Ok(catalog) = self.document.catalog() {
            if let Ok(m) = catalog.get(b"Metadata") {
                self.leaf(m, Role::Metadata);
            }
            if let Ok(o) = catalog.get(b"OutputIntents") {
                self.output_intents(o);
            }
            if let Some((_, form)) = catalog.get(b"AcroForm").ok().and_then(|o| self.dict(o)) {
                let resources = form
                    .get(b"DR")
                    .ok()
                    .map(|r| self.scope(r, None))
                    .unwrap_or(ResourceScope::NONE);
                let appearance = form.get(b"DA").ok();
                if let Ok(fields) = form.get(b"Fields") {
                    for field in self.items(fields) {
                        self.field(field, resources, appearance, 0);
                    }
                }
            }
            if let Some((_, names)) = catalog.get(b"Names").ok().and_then(|o| self.dict(o))
                && let Ok(tree) = names.get(b"AP")
            {
                self.appearance_tree(tree, 0);
            }
        }
        while self.complete
            && let Some((ids, scope)) = self.queue.pop_front()
        {
            self.read(&ids, scope);
        }
    }

    /// An inherited page attribute, unresolved, so the walk can tell shared objects apart. A page tree deeper than the
    /// walk follows leaves the walk incomplete rather than the page without its resources.
    pub(super) fn inherited(&mut self, page: &'a Dictionary, key: &[u8]) -> Option<&'a Object> {
        let mut current = page;
        for _ in 0..WALK_DEPTH {
            if let Ok(value) = current.get(key) {
                return Some(value);
            }
            current = current
                .get(b"Parent")
                .ok()
                .and_then(|p| self.dict(p))
                .map(|(_, d)| d)?;
        }
        self.complete = false;
        None
    }

    pub(super) fn page(&mut self, id: ObjectId) {
        let Ok(page) = self.document.get_dictionary(id) else {
            return;
        };
        if !self.enter(Some(id)) {
            return;
        }
        let scope = self
            .inherited(page, b"Resources")
            .map(|r| self.scope(r, Some(id)))
            .unwrap_or(ResourceScope::NONE);
        let mut contents = Vec::new();
        if let Ok(value) = page.get(b"Contents") {
            let parts = match self.get(value) {
                Some((_, Object::Array(items))) => items.iter().collect(),
                _ => vec![value],
            };
            for part in parts {
                if let Some((Some(stream), _)) = self.stream(part) {
                    contents.push(stream);
                }
            }
        }
        if !contents.is_empty() {
            self.queue_content(contents, scope);
        }
        if let Ok(annotations) = page.get(b"Annots") {
            for annotation in self.items(annotations) {
                self.annotation(annotation, scope);
            }
        }
        if let Ok(t) = page.get(b"Thumb") {
            self.image(t);
        }
        if let Ok(m) = page.get(b"Metadata") {
            self.leaf(m, Role::Metadata);
        }
        if let Ok(g) = page.get(b"Group") {
            self.group(g);
        }
        if let Ok(o) = page.get(b"OutputIntents") {
            self.output_intents(o);
        }
    }

    /// Reads queued content: decodes it, lexes it, and follows each resource an operation names, by its category and
    /// in this scope's resource dictionary. Inline images are kept for the scan.
    pub(super) fn read(&mut self, ids: &[ObjectId], scope: ResourceScope<'a>) {
        let mut bytes = Vec::new();
        for id in ids {
            let Some(Object::Stream(stream)) = self.document.objects.get(id) else {
                continue;
            };
            let left = CONTENT_LIMIT.saturating_sub(self.content_bytes);
            // A stream that cannot be decoded is counted by the scan, which holds the file; past the budget the
            // walk is incomplete.
            let Some(decoded) = decode(stream, STREAM_LIMIT.min(left)) else {
                if left < STREAM_LIMIT {
                    self.complete = false;
                }
                continue;
            };
            self.content_bytes += decoded.len();
            bytes.extend_from_slice(&decoded);
            bytes.push(b'\n');
        }
        let parsed = match content::parse(&bytes, &mut self.operations) {
            Ok(parsed) => parsed,
            Err(content::Error::Budget) => {
                self.complete = false;
                return;
            }
            Err(content::Error::InlineImage) => {
                self.unreadable += 1;
                return;
            }
        };
        self.dropped += parsed.dropped;
        self.collect_strings(ids, &parsed.items);
        for item in &parsed.items {
            for (category, name) in content::uses(item) {
                let Some(member) = scope
                    .resources
                    .and_then(|r| r.get(category.key()).ok())
                    .and_then(|c| self.dict(c))
                    .and_then(|(_, c)| c.get(name).ok())
                else {
                    continue;
                };
                match category {
                    Category::XObject => self.xobject(member, scope),
                    Category::Font => self.font(member, scope),
                    Category::ExtGState => self.extgstate(member, scope),
                    Category::Shading => self.shading(member),
                    Category::ColorSpace => self.colorspace(member),
                    Category::Pattern => self.pattern(member, scope),
                    Category::Properties => self.property(member),
                }
            }
            if let Item::Image(image) = item {
                self.inline
                    .push(inline_stream(self.document, image, scope.resources));
            }
        }
    }

    pub(super) fn xobject(&mut self, object: &'a Object, scope: ResourceScope<'a>) {
        let Some((_, stream)) = self.stream(object) else {
            return;
        };
        match name(&stream.dict, b"Subtype") {
            Some(b"Image") => self.image(object),
            Some(b"Form") => self.form(object, scope),
            _ => {}
        }
    }

    pub(super) fn image(&mut self, object: &'a Object) {
        let Some((id, stream)) = self.stream(object) else {
            return;
        };
        self.mark(id, Role::Image);
        if !self.enter(id) {
            return;
        }
        let dict = &stream.dict;
        for key in [&b"SMask"[..], b"Mask"] {
            if let Ok(mask) = dict.get(key) {
                self.image(mask);
            }
        }
        if let Ok(alternates) = dict.get(b"Alternates") {
            for alternate in self.items(alternates) {
                if let Some(image) = self.dict(alternate).and_then(|(_, d)| d.get(b"Image").ok()) {
                    self.image(image);
                }
            }
        }
        if let Ok(cs) = dict.get(b"ColorSpace") {
            self.colorspace(cs);
        }
        if let Ok(m) = dict.get(b"Metadata") {
            self.leaf(m, Role::Metadata);
        }
        if let Ok(parms) = dict.get(b"DecodeParms") {
            let all = match self.get(parms) {
                Some((_, Object::Array(items))) => items.iter().collect(),
                _ => vec![parms],
            };
            for p in all {
                if let Some(globals) = self.dict(p).and_then(|(_, d)| d.get(b"JBIG2Globals").ok()) {
                    self.leaf(globals, Role::Data);
                }
            }
        }
    }

    /// A form XObject: its content is read in its own resources, or its caller's when it has none.
    pub(super) fn form(&mut self, object: &'a Object, parent: ResourceScope<'a>) {
        let Some((Some(id), stream)) = self.stream(object) else {
            return;
        };
        let scope = match stream.dict.get(b"Resources") {
            Ok(r) => self.scope(r, Some(id)),
            Err(_) => parent,
        };
        self.queue_content(vec![id], scope);
        if !self.enter(Some(id)) {
            return;
        }
        if let Ok(g) = stream.dict.get(b"Group") {
            self.group(g);
        }
        if let Ok(m) = stream.dict.get(b"Metadata") {
            self.leaf(m, Role::Metadata);
        }
    }

    pub(super) fn group(&mut self, object: &'a Object) {
        if let Some(cs) = self.dict(object).and_then(|(_, d)| d.get(b"CS").ok()) {
            self.colorspace(cs);
        }
    }

    pub(super) fn font(&mut self, object: &'a Object, parent: ResourceScope<'a>) {
        let Some((id, font)) = self.dict(object) else {
            return;
        };
        // A Type 3 font draws each glyph with a content stream, read in the font's resources or its caller's.
        if let Ok(procs) = font.get(b"CharProcs") {
            let scope = match font.get(b"Resources") {
                Ok(r) => self.scope(r, id),
                Err(_) => parent,
            };
            let glyphs: Vec<ObjectId> = self
                .values(procs)
                .into_iter()
                .filter_map(|g| self.stream(g).and_then(|(gid, _)| gid))
                .collect();
            for glyph in glyphs {
                self.queue_content(vec![glyph], scope);
            }
        }
        if !self.enter(id) {
            return;
        }
        if let Ok(d) = font.get(b"FontDescriptor") {
            self.font_descriptor(d);
        }
        if let Ok(t) = font.get(b"ToUnicode") {
            self.leaf(t, Role::Data);
        }
        if let Ok(e) = font.get(b"Encoding") {
            self.cmap(e);
        }
        if let Ok(m) = font.get(b"CIDToGIDMap") {
            self.leaf(m, Role::Data);
        }
        if let Ok(descendants) = font.get(b"DescendantFonts") {
            for descendant in self.items(descendants) {
                self.font(descendant, parent);
            }
        }
    }

    pub(super) fn cmap(&mut self, object: &'a Object) {
        let Some((id, stream)) = self.stream(object) else {
            return;
        };
        self.mark(id, Role::Data);
        if !self.enter(id) {
            return;
        }
        if let Ok(parent) = stream.dict.get(b"UseCMap") {
            self.cmap(parent);
        }
    }

    pub(super) fn font_descriptor(&mut self, object: &'a Object) {
        let Some((id, descriptor)) = self.dict(object) else {
            return;
        };
        if !self.enter(id) {
            return;
        }
        for key in [&b"FontFile"[..], b"FontFile2", b"FontFile3"] {
            if let Ok(file) = descriptor.get(key) {
                self.leaf(file, Role::Font);
            }
        }
        if let Ok(set) = descriptor.get(b"CIDSet") {
            self.leaf(set, Role::Data);
        }
        if let Ok(fd) = descriptor.get(b"FD") {
            for inner in self.values(fd) {
                self.font_descriptor(inner);
            }
        }
    }

    pub(super) fn colorspace(&mut self, object: &'a Object) {
        let Some((id, Object::Array(items))) = self.get(object) else {
            return;
        };
        if !self.enter(id) {
            return;
        }
        match items.first().and_then(|f| f.as_name().ok()) {
            Some(b"ICCBased") => {
                if let Some(profile) = items.get(1)
                    && let Some((pid, stream)) = self.stream(profile)
                {
                    self.mark(pid, Role::Data);
                    if let Ok(alternate) = stream.dict.get(b"Alternate") {
                        self.colorspace(alternate);
                    }
                }
            }
            Some(b"Indexed" | b"I") => {
                if let Some(base) = items.get(1) {
                    self.colorspace(base);
                }
                if let Some(lookup) = items.get(3) {
                    self.leaf(lookup, Role::Data);
                }
            }
            Some(b"Separation" | b"DeviceN") => {
                if let Some(alternate) = items.get(2) {
                    self.colorspace(alternate);
                }
                if let Some(tint) = items.get(3) {
                    self.function(tint);
                }
                if let Some((_, attributes)) = items.get(4).and_then(|a| self.dict(a)) {
                    if let Ok(colorants) = attributes.get(b"Colorants") {
                        for cs in self.values(colorants) {
                            self.colorspace(cs);
                        }
                    }
                    if let Some(cs) = attributes
                        .get(b"Process")
                        .ok()
                        .and_then(|p| self.dict(p))
                        .and_then(|(_, p)| p.get(b"ColorSpace").ok())
                    {
                        self.colorspace(cs);
                    }
                }
            }
            Some(b"Pattern") => {
                if let Some(base) = items.get(1) {
                    self.colorspace(base);
                }
            }
            _ => {}
        }
    }

    pub(super) fn function(&mut self, object: &'a Object) {
        match self.get(object) {
            Some((id, Object::Stream(_))) => {
                self.mark(id, Role::Data);
                self.enter(id);
            }
            Some((id, Object::Dictionary(d))) => {
                if self.enter(id)
                    && let Ok(inner) = d.get(b"Functions")
                {
                    self.function(inner);
                }
            }
            Some((_, Object::Array(items))) => {
                for item in items {
                    self.function(item);
                }
            }
            _ => {}
        }
    }

    pub(super) fn pattern(&mut self, object: &'a Object, parent: ResourceScope<'a>) {
        match self.get(object) {
            // A tiling pattern's cell is content, read in the pattern's resources.
            Some((Some(id), Object::Stream(stream))) => {
                let scope = match stream.dict.get(b"Resources") {
                    Ok(r) => self.scope(r, Some(id)),
                    Err(_) => parent,
                };
                self.queue_content(vec![id], scope);
            }
            Some((id, Object::Dictionary(d))) => {
                if !self.enter(id) {
                    return;
                }
                if let Ok(s) = d.get(b"Shading") {
                    self.shading(s);
                }
                if let Ok(g) = d.get(b"ExtGState") {
                    self.extgstate(g, parent);
                }
            }
            _ => {}
        }
    }

    pub(super) fn shading(&mut self, object: &'a Object) {
        let (id, dict) = match self.get(object) {
            Some((id, Object::Stream(s))) => (id, &s.dict),
            Some((id, Object::Dictionary(d))) => (id, d),
            _ => return,
        };
        if matches!(self.get(object), Some((_, Object::Stream(_)))) {
            self.mark(id, Role::Data);
        }
        if !self.enter(id) {
            return;
        }
        if let Ok(f) = dict.get(b"Function") {
            self.function(f);
        }
        if let Ok(cs) = dict.get(b"ColorSpace") {
            self.colorspace(cs);
        }
    }

    pub(super) fn extgstate(&mut self, object: &'a Object, scope: ResourceScope<'a>) {
        let Some((id, state)) = self.dict(object) else {
            return;
        };
        // A soft mask's group is a form, read in its own resources or this state's caller's.
        if let Some((_, mask)) = state.get(b"SMask").ok().and_then(|m| self.dict(m)) {
            if let Ok(g) = mask.get(b"G") {
                self.form(g, scope);
            }
            if let Ok(tr) = mask.get(b"TR") {
                self.function(tr);
            }
        }
        if !self.enter(id) {
            return;
        }
        for key in [&b"TR"[..], b"TR2", b"BG", b"BG2", b"UCR", b"UCR2"] {
            if let Ok(f) = state.get(key) {
                self.function(f);
            }
        }
        if let Ok(ht) = state.get(b"HT") {
            self.halftone(ht, 0);
        }
        if let Some(font) = state.get(b"Font").ok().and_then(|f| self.items(f).first()) {
            self.font(font, scope);
        }
    }

    pub(super) fn halftone(&mut self, object: &'a Object, depth: usize) {
        // A type 5 halftone names one halftone per colorant, one level down; deeper is malformed.
        if depth > 4 {
            self.complete = false;
            return;
        }
        let (id, dict) = match self.get(object) {
            Some((id, Object::Stream(s))) => (id, &s.dict),
            Some((id, Object::Dictionary(d))) => (id, d),
            _ => return,
        };
        if matches!(self.get(object), Some((_, Object::Stream(_)))) {
            self.mark(id, Role::Data);
        }
        if !self.enter(id) {
            return;
        }
        for (key, value) in dict.iter() {
            match key.as_slice() {
                b"SpotFunction" | b"TransferFunction" => self.function(value),
                b"Type" | b"HalftoneType" | b"HalftoneName" | b"Frequency" | b"Angle"
                | b"AccurateScreens" | b"Width" | b"Height" | b"Width2" | b"Height2" => {}
                // A type 5 halftone names one halftone per colorant.
                _ => self.halftone(value, depth + 1),
            }
        }
    }

    pub(super) fn property(&mut self, object: &'a Object) {
        if let Some((id, property)) = self.dict(object)
            && self.enter(id)
            && let Ok(m) = property.get(b"Metadata")
        {
            self.leaf(m, Role::Metadata);
        }
    }

    /// An annotation's appearances and icons, read in the page's resources when they have none of their own.
    pub(super) fn annotation(&mut self, object: &'a Object, scope: ResourceScope<'a>) {
        let Some((id, annotation)) = self.dict(object) else {
            return;
        };
        if let Ok(appearances) = annotation.get(b"AP") {
            for state in self.values(appearances) {
                match self.get(state) {
                    Some((_, Object::Stream(_))) => self.form(state, scope),
                    Some((_, Object::Dictionary(states))) => {
                        for form in states.iter().map(|(_, v)| v) {
                            self.form(form, scope);
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Some((_, characteristics)) = annotation.get(b"MK").ok().and_then(|m| self.dict(m)) {
            for key in [&b"I"[..], b"RI", b"IX"] {
                if let Ok(icon) = characteristics.get(key) {
                    self.form(icon, scope);
                }
            }
        }
        if !self.enter(id) {
            return;
        }
        if let Ok(rich) = annotation.get(b"RC") {
            self.leaf(rich, Role::Data);
        }
    }

    /// A form field: the fonts its default appearance string names, in the form's default resources, then its
    /// widgets and kids.
    pub(super) fn field(
        &mut self,
        object: &'a Object,
        resources: ResourceScope<'a>,
        appearance: Option<&'a Object>,
        depth: usize,
    ) {
        if depth > WALK_DEPTH {
            self.complete = false;
            return;
        }
        let Some((_, field)) = self.dict(object) else {
            return;
        };
        let appearance = field.get(b"DA").ok().or(appearance);
        if let Some(Object::String(da, _)) = appearance.and_then(|a| deref(self.document, a))
            && let Ok(parsed) = content::parse(da, &mut self.operations)
        {
            for item in &parsed.items {
                for (category, name) in content::uses(item) {
                    if category == Category::Font
                        && let Some(font) = resources
                            .resources
                            .and_then(|r| r.get(b"Font").ok())
                            .and_then(|f| self.dict(f))
                            .and_then(|(_, f)| f.get(name).ok())
                    {
                        self.font(font, resources);
                    }
                }
            }
        }
        if let Ok(kids) = field.get(b"Kids") {
            for kid in self.items(kids) {
                self.field(kid, resources, appearance, depth + 1);
            }
        }
        self.annotation(object, resources);
    }

    pub(super) fn output_intents(&mut self, object: &'a Object) {
        for intent in self.items(object) {
            if let Some(profile) = self
                .dict(intent)
                .and_then(|(_, d)| d.get(b"DestOutputProfile").ok())
            {
                self.leaf(profile, Role::Data);
            }
        }
    }

    pub(super) fn appearance_tree(&mut self, object: &'a Object, depth: usize) {
        if depth > WALK_DEPTH {
            self.complete = false;
            return;
        }
        let Some((id, node)) = self.dict(object) else {
            return;
        };
        if !self.enter(id) {
            return;
        }
        if let Ok(names) = node.get(b"Names") {
            for value in self.items(names).iter().skip(1).step_by(2) {
                self.form(value, ResourceScope::NONE);
            }
        }
        if let Ok(kids) = node.get(b"Kids") {
            for kid in self.items(kids) {
                self.appearance_tree(kid, depth + 1);
            }
        }
    }
}

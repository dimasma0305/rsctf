//! Bounded, best-effort PDF text recovery for artifact scanning.
//!
//! This is not a renderer. It finds indirect objects (including those packed
//! in object streams), decodes streams through their text filter chains under
//! hard size and ratio caps, and turns page content into text, following Form
//! XObjects such as the wrapped pages `pdfjam` produces. Glyph-ID strings are
//! decoded with each font's `ToUnicode` CMap, which is how browser- and
//! LaTeX-produced PDFs store text. Text drawn as vector outlines or images, or
//! behind a font without a `ToUnicode` map, cannot be recovered and is missed.

use std::collections::{HashMap, HashSet};

use super::pdf_filters::StreamDecoder;

const MAX_OBJECTS: usize = 100_000;
const MAX_PAGES: usize = 2_000;
const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;
const MAX_DEPTH: usize = 32;
/// Nesting of Form XObjects drawn from page content.
const MAX_FORM_DEPTH: usize = 8;
const MAX_CMAP_ENTRIES: usize = 65_536;
/// Map writes (including overwrites) allowed across every CMap of a document.
const MAX_CMAP_WORK: usize = 1_000_000;
/// Page content bytes lexed across the whole document.
const MAX_CONTENT_BYTES: usize = 64 * 1024 * 1024;

/// Every decoded stream plus the recovered page text.
#[derive(Debug, Default)]
pub(super) struct PdfContent {
    pub streams: Vec<Vec<u8>>,
    pub text: Vec<u8>,
}

pub(super) fn is_pdf(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(1024)];
    head.windows(5).any(|window| window == b"%PDF-")
}

// ─── Lexer ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Num(f64),
    Name(Vec<u8>),
    Str(Vec<u8>),
    ArrStart,
    ArrEnd,
    DictStart,
    DictEnd,
    Keyword(Vec<u8>),
}

fn is_white(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | b'\x0c' | b'\0')
}

fn is_delim(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

struct Lexer<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn skip_space(&mut self) {
        while self.pos < self.data.len() {
            let byte = self.data[self.pos];
            if is_white(byte) {
                self.pos += 1;
            } else if byte == b'%' {
                while self.pos < self.data.len() && !matches!(self.data[self.pos], b'\r' | b'\n') {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    fn literal(&mut self) -> Vec<u8> {
        // Positioned after the opening parenthesis.
        let mut out = Vec::new();
        let mut depth = 1usize;
        while self.pos < self.data.len() {
            let byte = self.data[self.pos];
            self.pos += 1;
            match byte {
                b'\\' => {
                    let Some(&next) = self.data.get(self.pos) else {
                        break;
                    };
                    self.pos += 1;
                    match next {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'\r' => {
                            if self.data.get(self.pos) == Some(&b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        b'0'..=b'7' => {
                            let mut value = u32::from(next - b'0');
                            for _ in 0..2 {
                                match self.data.get(self.pos) {
                                    Some(&digit @ b'0'..=b'7') => {
                                        value = value * 8 + u32::from(digit - b'0');
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push((value & 0xff) as u8);
                        }
                        other => out.push(other),
                    }
                }
                b'(' => {
                    depth += 1;
                    out.push(byte);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(byte);
                }
                _ => out.push(byte),
            }
        }
        out
    }

    fn hex(&mut self) -> Vec<u8> {
        // Positioned after the opening angle bracket.
        let mut out = Vec::new();
        let mut high: Option<u8> = None;
        while self.pos < self.data.len() {
            let byte = self.data[self.pos];
            self.pos += 1;
            if byte == b'>' {
                break;
            }
            let nibble = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => continue,
            };
            match high.take() {
                Some(value) => out.push(value << 4 | nibble),
                None => high = Some(nibble),
            }
        }
        if let Some(value) = high {
            out.push(value << 4);
        }
        out
    }

    fn next_token(&mut self) -> Option<Token> {
        self.skip_space();
        let byte = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(match byte {
            b'(' => Token::Str(self.literal()),
            b'<' if self.data.get(self.pos) == Some(&b'<') => {
                self.pos += 1;
                Token::DictStart
            }
            b'<' => Token::Str(self.hex()),
            b'>' if self.data.get(self.pos) == Some(&b'>') => {
                self.pos += 1;
                Token::DictEnd
            }
            b'[' => Token::ArrStart,
            b']' => Token::ArrEnd,
            b'/' => {
                let start = self.pos;
                while self.pos < self.data.len()
                    && !is_white(self.data[self.pos])
                    && !is_delim(self.data[self.pos])
                {
                    self.pos += 1;
                }
                Token::Name(self.data[start..self.pos].to_vec())
            }
            b'{' | b'}' | b')' | b'>' => Token::Keyword(vec![byte]),
            _ => {
                let start = self.pos - 1;
                while self.pos < self.data.len()
                    && !is_white(self.data[self.pos])
                    && !is_delim(self.data[self.pos])
                {
                    self.pos += 1;
                }
                let word = &self.data[start..self.pos];
                match std::str::from_utf8(word)
                    .ok()
                    .and_then(|text| text.parse::<f64>().ok())
                {
                    Some(value) if word[0] != b'+' || word.len() > 1 => Token::Num(value),
                    _ => Token::Keyword(word.to_vec()),
                }
            }
        })
    }

    /// Skip inline-image data after `ID` up to the terminating `EI`.
    fn skip_inline_image(&mut self) {
        while self.pos + 2 < self.data.len() {
            if is_white(self.data[self.pos])
                && self.data[self.pos + 1] == b'E'
                && self.data[self.pos + 2] == b'I'
                && self
                    .data
                    .get(self.pos + 3)
                    .is_none_or(|byte| is_white(*byte) || is_delim(*byte))
            {
                self.pos += 3;
                return;
            }
            self.pos += 1;
        }
        self.pos = self.data.len();
    }
}

// ─── Object values ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum Value {
    Num(f64),
    Name(Vec<u8>),
    Ref(u32),
    Arr(Vec<Value>),
    Dict(Vec<(Vec<u8>, Value)>),
    Other,
}

impl Value {
    fn get(&self, key: &[u8]) -> Option<&Value> {
        match self {
            Value::Dict(entries) => entries
                .iter()
                .find(|(name, _)| name.as_slice() == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    fn name_is(&self, key: &[u8], expected: &[u8]) -> bool {
        matches!(self.get(key), Some(Value::Name(name)) if name.as_slice() == expected)
    }
}

fn parse_value(tokens: &[Token], index: &mut usize, depth: usize) -> Value {
    if depth > MAX_DEPTH || *index >= tokens.len() {
        return Value::Other;
    }
    let token = tokens[*index].clone();
    *index += 1;
    match token {
        Token::Num(value) => {
            // `N G R` is an indirect reference.
            if let (Some(Token::Num(_)), Some(Token::Keyword(word))) =
                (tokens.get(*index), tokens.get(*index + 1))
            {
                if word == b"R" && value >= 0.0 && value.fract() == 0.0 && value < 4e9 {
                    *index += 2;
                    return Value::Ref(value as u32);
                }
            }
            Value::Num(value)
        }
        Token::Name(name) => Value::Name(name),
        Token::ArrStart => {
            let mut items = Vec::new();
            while *index < tokens.len() && tokens[*index] != Token::ArrEnd {
                items.push(parse_value(tokens, index, depth + 1));
            }
            *index += 1;
            Value::Arr(items)
        }
        Token::DictStart => {
            let mut entries = Vec::new();
            while *index < tokens.len() && tokens[*index] != Token::DictEnd {
                match tokens[*index].clone() {
                    Token::Name(key) => {
                        *index += 1;
                        let value = parse_value(tokens, index, depth + 1);
                        entries.push((key, value));
                    }
                    _ => *index += 1,
                }
            }
            *index += 1;
            Value::Dict(entries)
        }
        _ => Value::Other,
    }
}

fn tokenize(data: &[u8], limit: usize) -> Vec<Token> {
    let mut lexer = Lexer::new(data);
    let mut tokens = Vec::new();
    while tokens.len() < limit {
        let Some(token) = lexer.next_token() else {
            break;
        };
        tokens.push(token);
    }
    tokens
}

fn parse_object(data: &[u8]) -> Value {
    let tokens = tokenize(data, 200_000);
    let mut index = 0;
    parse_value(&tokens, &mut index, 0)
}

// ─── Objects and streams ─────────────────────────────────────────────────

#[derive(Debug)]
struct PdfObject {
    dict: Value,
    stream: Option<Vec<u8>>,
}

/// A stream's filter names in the order they apply, or `None` when the
/// `Filter` entry is neither a name nor an array of names.
fn filter_names(dict: &Value) -> Option<Vec<&[u8]>> {
    match dict.get(b"Filter") {
        None => Some(Vec::new()),
        Some(Value::Name(name)) => Some(vec![name.as_slice()]),
        Some(Value::Arr(items)) => items
            .iter()
            .map(|item| match item {
                Value::Name(name) => Some(name.as_slice()),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|position| position + from)
}

/// Locate `N G obj` headers by scanning backwards from each `obj` keyword.
fn object_starts(data: &[u8]) -> Vec<(u32, usize)> {
    let mut starts = Vec::new();
    let mut from = 0;
    while let Some(at) = find(data, b"obj", from) {
        from = at + 3;
        if starts.len() >= MAX_OBJECTS {
            break;
        }
        if data
            .get(at + 3)
            .is_some_and(|byte| !is_white(*byte) && !is_delim(*byte))
        {
            continue;
        }
        // Expect "<digits> <digits> obj" immediately before.
        let mut cursor = at;
        let mut numbers = Vec::new();
        for _ in 0..2 {
            while cursor > 0 && is_white(data[cursor - 1]) {
                cursor -= 1;
            }
            let end = cursor;
            while cursor > 0 && data[cursor - 1].is_ascii_digit() {
                cursor -= 1;
            }
            if cursor == end {
                break;
            }
            numbers.push(&data[cursor..end]);
        }
        if numbers.len() == 2 {
            if let Some(number) = std::str::from_utf8(numbers[1])
                .ok()
                .and_then(|text| text.parse::<u32>().ok())
            {
                starts.push((number, at + 3));
            }
        }
    }
    starts
}

fn read_objects(data: &[u8], decoder: &mut StreamDecoder) -> HashMap<u32, PdfObject> {
    let mut objects = HashMap::new();
    let starts = object_starts(data);
    for (index, &(number, body_start)) in starts.iter().enumerate() {
        // Bound every search by the next object header so a file without
        // `endobj` keywords cannot make each object rescan the whole file.
        let limit = starts
            .get(index + 1)
            .map(|(_, next)| *next)
            .unwrap_or(data.len())
            .max(body_start);
        let body_end = find(&data[..limit], b"endobj", body_start).unwrap_or(limit);
        let body = &data[body_start..body_end];
        let (dict_bytes, stream_bytes) = match find(body, b"stream", 0) {
            Some(at) if !body[..at].ends_with(b"end") => {
                let mut start = at + 6;
                if body.get(start) == Some(&b'\r') {
                    start += 1;
                }
                if body.get(start) == Some(&b'\n') {
                    start += 1;
                }
                let end = find(body, b"endstream", start).unwrap_or(body.len());
                (&body[..at], Some(&body[start..end.max(start)]))
            }
            _ => (body, None),
        };
        let dict = parse_object(dict_bytes);
        let stream = stream_bytes.and_then(|raw| decoder.decode(raw, &filter_names(&dict)?));
        objects.insert(number, PdfObject { dict, stream });
    }
    // Objects packed into object streams (PDF 1.5+).
    let packed = objects
        .values()
        .filter(|object| object.dict.name_is(b"Type", b"ObjStm"))
        .filter_map(|object| {
            let count = match object.dict.get(b"N") {
                Some(Value::Num(value)) => *value as usize,
                _ => return None,
            };
            let first = match object.dict.get(b"First") {
                Some(Value::Num(value)) => *value as usize,
                _ => return None,
            };
            object
                .stream
                .as_ref()
                .map(|bytes| (count, first, bytes.clone()))
        })
        .collect::<Vec<_>>();
    for (count, first, bytes) in packed {
        let header = tokenize(&bytes[..first.min(bytes.len())], count.min(MAX_OBJECTS) * 2);
        let pairs = header
            .chunks(2)
            .filter_map(|pair| match pair {
                [Token::Num(number), Token::Num(offset)] => {
                    Some((*number as u32, *offset as usize))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for (index, (number, offset)) in pairs.iter().enumerate() {
            let start = first.saturating_add(*offset).min(bytes.len());
            let end = pairs
                .get(index + 1)
                .map(|(_, next)| first.saturating_add(*next).min(bytes.len()))
                .unwrap_or(bytes.len())
                .max(start);
            if objects.len() >= MAX_OBJECTS {
                break;
            }
            objects.entry(*number).or_insert_with(|| PdfObject {
                dict: parse_object(&bytes[start..end]),
                stream: None,
            });
        }
    }
    objects
}

fn resolve<'a>(objects: &'a HashMap<u32, PdfObject>, value: &'a Value) -> Option<&'a Value> {
    let mut current = value;
    for _ in 0..MAX_DEPTH {
        match current {
            Value::Ref(number) => current = &objects.get(number)?.dict,
            other => return Some(other),
        }
    }
    None
}

// ─── ToUnicode CMaps ─────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct CMap {
    code_len: usize,
    map: HashMap<u32, String>,
}

fn utf16(bytes: &[u8]) -> String {
    let units = bytes
        .chunks(2)
        .map(|pair| u16::from_be_bytes([pair[0], *pair.get(1).unwrap_or(&0)]))
        .collect::<Vec<_>>();
    String::from_utf16_lossy(&units)
}

fn code_of(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .take(4)
        .fold(0u32, |acc, byte| acc << 8 | u32::from(*byte))
}

/// Record one mapping within the document-wide work budget. Returns false
/// once the budget or the entry cap is exhausted, which ends parsing: repeated
/// overlapping ranges would otherwise loop without growing the map.
fn put(cmap: &mut CMap, work: &mut usize, code: u32, value: String) -> bool {
    if *work == 0 || cmap.map.len() >= MAX_CMAP_ENTRIES {
        return false;
    }
    *work -= 1;
    cmap.map.insert(code, value);
    true
}

fn parse_cmap(data: &[u8], work: &mut usize) -> CMap {
    let tokens = tokenize(data, 2_000_000);
    let mut cmap = CMap {
        code_len: 1,
        map: HashMap::new(),
    };
    let mut index = 0;
    'tokens: while index < tokens.len() {
        match &tokens[index] {
            Token::Keyword(word) if word == b"begincodespacerange" => {
                if let Some(Token::Str(low)) = tokens.get(index + 1) {
                    cmap.code_len = low.len().clamp(1, 4);
                }
                index += 1;
            }
            Token::Keyword(word) if word == b"beginbfchar" => {
                index += 1;
                while let (Some(Token::Str(source)), Some(Token::Str(target))) =
                    (tokens.get(index), tokens.get(index + 1))
                {
                    cmap.code_len = source.len().clamp(1, 4);
                    if !put(&mut cmap, work, code_of(source), utf16(target)) {
                        break 'tokens;
                    }
                    index += 2;
                }
            }
            Token::Keyword(word) if word == b"beginbfrange" => {
                index += 1;
                while let (Some(Token::Str(low)), Some(Token::Str(high))) =
                    (tokens.get(index), tokens.get(index + 1))
                {
                    cmap.code_len = low.len().clamp(1, 4);
                    let (low, high) = (code_of(low), code_of(high));
                    match tokens.get(index + 2) {
                        Some(Token::Str(target)) => {
                            let mut units = target.clone();
                            for code in low..=high.min(low.saturating_add(MAX_CMAP_ENTRIES as u32))
                            {
                                if !put(&mut cmap, work, code, utf16(&units)) {
                                    break 'tokens;
                                }
                                if let Some(last) = units.last_mut() {
                                    *last = last.wrapping_add(1);
                                }
                            }
                            index += 3;
                        }
                        Some(Token::ArrStart) => {
                            let mut code = low;
                            index += 3;
                            while let Some(Token::Str(target)) = tokens.get(index) {
                                if !put(&mut cmap, work, code, utf16(target)) {
                                    break 'tokens;
                                }
                                code = code.saturating_add(1);
                                index += 1;
                            }
                            index += 1;
                        }
                        _ => index += 2,
                    }
                }
            }
            _ => index += 1,
        }
    }
    cmap
}

// ─── Page text ───────────────────────────────────────────────────────────

fn decode_string(bytes: &[u8], cmap: Option<&CMap>) -> String {
    match cmap {
        Some(cmap) if !cmap.map.is_empty() => bytes
            .chunks(cmap.code_len)
            .map(|chunk| {
                cmap.map
                    .get(&code_of(chunk))
                    .cloned()
                    .unwrap_or_else(|| "\u{fffd}".to_string())
            })
            .collect(),
        // Simple fonts without a ToUnicode map: treat as Latin-1.
        _ => bytes.iter().map(|byte| char::from(*byte)).collect(),
    }
}

/// Resources in effect for a page: its own, or the nearest ancestor's.
fn page_resources<'a>(objects: &'a HashMap<u32, PdfObject>, page: &'a Value) -> Option<&'a Value> {
    let mut node = Some(page);
    for _ in 0..MAX_DEPTH {
        let current = node?;
        if let Some(resources) = current
            .get(b"Resources")
            .and_then(|value| resolve(objects, value))
        {
            return Some(resources);
        }
        node = current
            .get(b"Parent")
            .and_then(|value| resolve(objects, value));
    }
    None
}

/// The named entries of one resource category, such as `Font` or `XObject`.
fn resource_entries<'a>(
    objects: &'a HashMap<u32, PdfObject>,
    resources: Option<&'a Value>,
    category: &[u8],
) -> &'a [(Vec<u8>, Value)] {
    match resources
        .and_then(|resources| resources.get(category))
        .and_then(|value| resolve(objects, value))
    {
        Some(Value::Dict(entries)) => entries,
        _ => &[],
    }
}

#[derive(Debug)]
enum Operand {
    Num(f64),
    Name(Vec<u8>),
    Str(Vec<u8>),
    Arr(Vec<Operand>),
}

fn read_operand(lexer: &mut Lexer<'_>, token: Token, depth: usize) -> Option<Operand> {
    Some(match token {
        Token::Num(value) => Operand::Num(value),
        Token::Name(name) => Operand::Name(name),
        Token::Str(bytes) => Operand::Str(bytes),
        Token::ArrStart if depth < 4 => {
            let mut items = Vec::new();
            while let Some(next) = lexer.next_token() {
                if next == Token::ArrEnd {
                    break;
                }
                if let Some(item) = read_operand(lexer, next, depth + 1) {
                    items.push(item);
                }
            }
            Operand::Arr(items)
        }
        _ => return None,
    })
}

/// Text recovery state shared by every page and form of one document, so
/// the budgets are per document and each stream is lexed at most once.
struct TextExtractor<'a> {
    objects: &'a HashMap<u32, PdfObject>,
    cmaps: HashMap<u32, CMap>,
    cmap_work: usize,
    seen: HashSet<u32>,
    budget: usize,
    text: Vec<u8>,
}

impl<'a> TextExtractor<'a> {
    fn new(objects: &'a HashMap<u32, PdfObject>) -> Self {
        Self {
            objects,
            cmaps: HashMap::new(),
            cmap_work: MAX_CMAP_WORK,
            seen: HashSet::new(),
            budget: MAX_CONTENT_BYTES,
            text: Vec::new(),
        }
    }

    fn done(&self) -> bool {
        self.budget == 0 || self.text.len() >= MAX_TEXT_BYTES
    }

    /// A content stream that has not been lexed yet, within the document's
    /// byte budget. Many pages may share one stream or form; each is lexed
    /// once so a small file cannot multiply the work.
    fn claim(&mut self, number: u32) -> Option<&'a [u8]> {
        if self.budget == 0 || !self.seen.insert(number) {
            return None;
        }
        let bytes = self.objects.get(&number)?.stream.as_deref()?;
        let take = bytes.len().min(self.budget);
        self.budget -= take;
        Some(&bytes[..take])
    }

    /// Font names mapped to their `ToUnicode` CMap object, parsing each CMap
    /// once within the document-wide work budget.
    fn fonts(&mut self, resources: Option<&'a Value>) -> HashMap<Vec<u8>, Option<u32>> {
        let objects = self.objects;
        let mut fonts = HashMap::new();
        for (name, font_ref) in resource_entries(objects, resources, b"Font") {
            let cmap_ref = resolve(objects, font_ref)
                .and_then(|font| font.get(b"ToUnicode"))
                .and_then(|value| match value {
                    Value::Ref(number) => Some(*number),
                    _ => None,
                });
            if let Some(number) = cmap_ref {
                if let Some(bytes) = objects
                    .get(&number)
                    .and_then(|object| object.stream.as_deref())
                {
                    let work = &mut self.cmap_work;
                    self.cmaps
                        .entry(number)
                        .or_insert_with(|| parse_cmap(bytes, work));
                }
            }
            fonts.entry(name.clone()).or_insert(cmap_ref);
        }
        fonts
    }

    fn page(&mut self, page: &'a Value) {
        let parts = match page.get(b"Contents") {
            Some(Value::Arr(parts)) => parts.iter().collect::<Vec<_>>(),
            Some(value) => vec![value],
            None => Vec::new(),
        };
        let mut content = Vec::new();
        for part in parts {
            let Value::Ref(number) = part else { continue };
            if let Some(bytes) = self.claim(*number) {
                content.extend_from_slice(bytes);
                content.push(b'\n');
            }
        }
        self.content(&content, page_resources(self.objects, page), 0);
        self.text.push(b'\n');
    }

    /// Draw a Form XObject named by `Do`: its content with its own resources,
    /// or the caller's when it has none.
    fn form(&mut self, name: &[u8], resources: Option<&'a Value>, depth: usize) {
        let objects = self.objects;
        let Some((_, Value::Ref(number))) = resource_entries(objects, resources, b"XObject")
            .iter()
            .find(|(key, _)| key.as_slice() == name)
        else {
            return;
        };
        let Some(object) = objects.get(number) else {
            return;
        };
        if !object.dict.name_is(b"Subtype", b"Form") {
            return;
        }
        let Some(content) = self.claim(*number) else {
            return;
        };
        let own = object
            .dict
            .get(b"Resources")
            .and_then(|value| resolve(objects, value))
            .or(resources);
        self.content(content, own, depth + 1);
    }

    fn content(&mut self, content: &[u8], resources: Option<&'a Value>, depth: usize) {
        let fonts = self.fonts(resources);
        let mut lexer = Lexer::new(content);
        let mut operands: Vec<Operand> = Vec::new();
        let mut font: Option<u32> = None;
        let mut line_y: Option<f64> = None;
        while let Some(token) = lexer.next_token() {
            if self.text.len() >= MAX_TEXT_BYTES {
                return;
            }
            let Token::Keyword(operator) = token else {
                if let Some(operand) = read_operand(&mut lexer, token, 0) {
                    if operands.len() < 64 {
                        operands.push(operand);
                    }
                }
                continue;
            };
            match operator.as_slice() {
                b"Tf" => {
                    font = operands.iter().rev().find_map(|operand| match operand {
                        Operand::Name(name) => fonts.get(name).copied().flatten(),
                        _ => None,
                    });
                }
                b"Tj" | b"'" | b"\"" => {
                    if matches!(operator.as_slice(), b"'" | b"\"") {
                        self.text.push(b'\n');
                    }
                    if let Some(Operand::Str(bytes)) = operands.last() {
                        let cmap = font.and_then(|number| self.cmaps.get(&number));
                        self.text
                            .extend_from_slice(decode_string(bytes, cmap).as_bytes());
                    }
                }
                b"TJ" => {
                    if let Some(Operand::Arr(items)) = operands.last() {
                        let cmap = font.and_then(|number| self.cmaps.get(&number));
                        for item in items {
                            match item {
                                Operand::Str(bytes) => self
                                    .text
                                    .extend_from_slice(decode_string(bytes, cmap).as_bytes()),
                                // A large negative adjustment is a visual word gap.
                                Operand::Num(value) if *value < -200.0 => self.text.push(b' '),
                                _ => {}
                            }
                        }
                    }
                }
                b"Td" | b"TD" | b"Tm" => {
                    let y = match operands.as_slice() {
                        [.., Operand::Num(y)] if operator.as_slice() == b"Tm" => Some(*y),
                        [.., Operand::Num(_), Operand::Num(y)] => Some(*y),
                        _ => None,
                    };
                    let new_line = match (operator.as_slice(), y) {
                        (b"Tm", Some(y)) => {
                            line_y.is_some_and(|previous| (previous - y).abs() > 0.5)
                        }
                        (_, Some(y)) => y.abs() > 0.5,
                        _ => true,
                    };
                    if operator.as_slice() == b"Tm" {
                        line_y = y;
                    }
                    if new_line {
                        self.text.push(b'\n');
                    }
                }
                b"T*" | b"ET" => self.text.push(b'\n'),
                b"Do" if depth < MAX_FORM_DEPTH => {
                    if let Some(Operand::Name(name)) = operands.last() {
                        self.form(name, resources, depth);
                    }
                }
                b"ID" => lexer.skip_inline_image(),
                _ => {}
            }
            operands.clear();
        }
    }
}

/// Decoded streams and recovered page text of a PDF, within fixed limits.
pub(super) fn extract(data: &[u8]) -> PdfContent {
    let mut decoder = StreamDecoder::new();
    let objects = read_objects(data, &mut decoder);
    let mut pages = objects
        .iter()
        .filter(|(_, object)| object.dict.name_is(b"Type", b"Page"))
        .map(|(number, _)| *number)
        .collect::<Vec<_>>();
    pages.sort_unstable();
    let mut text = {
        let mut extractor = TextExtractor::new(&objects);
        for number in pages.into_iter().take(MAX_PAGES) {
            extractor.page(&objects[&number].dict);
            if extractor.done() {
                break;
            }
        }
        extractor.text
    };
    text.truncate(MAX_TEXT_BYTES);
    PdfContent {
        streams: objects
            .into_values()
            .filter_map(|object| object.stream)
            .collect(),
        text,
    }
}

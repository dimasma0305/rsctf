//! Bounded, best-effort PDF text recovery for artifact scanning.
//!
//! This is not a renderer. It finds indirect objects (including those packed
//! in object streams), inflates `FlateDecode` streams under hard size and ratio
//! caps, and turns page content into text. Glyph-ID strings are decoded with
//! each page font's `ToUnicode` CMap, which is how browser- and LaTeX-produced
//! PDFs store text. Text drawn as vector outlines or behind a font without a
//! `ToUnicode` map cannot be recovered and is simply missed.

use std::collections::HashMap;
use std::io::Read;

/// Largest decoded size of one stream.
const MAX_STREAM_BYTES: usize = 8 * 1024 * 1024;
/// Largest decoded size of all streams in one document.
const MAX_TOTAL_INFLATED: usize = 64 * 1024 * 1024;
/// A stream may not inflate beyond this multiple of its compressed size
/// (plus a small allowance), which defeats decompression bombs early.
const MAX_INFLATE_RATIO: usize = 200;
const INFLATE_ALLOWANCE: usize = 64 * 1024;
const MAX_OBJECTS: usize = 100_000;
const MAX_PAGES: usize = 2_000;
const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;
const MAX_DEPTH: usize = 32;
const MAX_CMAP_ENTRIES: usize = 65_536;

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

struct Inflater {
    total: usize,
}

impl Inflater {
    fn inflate(&mut self, compressed: &[u8]) -> Option<Vec<u8>> {
        let cap = compressed
            .len()
            .saturating_mul(MAX_INFLATE_RATIO)
            .saturating_add(INFLATE_ALLOWANCE)
            .min(MAX_STREAM_BYTES)
            .min(MAX_TOTAL_INFLATED.saturating_sub(self.total));
        if cap == 0 {
            return None;
        }
        let mut out = Vec::new();
        let mut decoder = flate2::read::ZlibDecoder::new(compressed).take(cap as u64);
        if decoder.read_to_end(&mut out).is_err() && out.is_empty() {
            // Raw deflate without a zlib header also appears in the wild.
            let mut raw = flate2::read::DeflateDecoder::new(compressed).take(cap as u64);
            out.clear();
            raw.read_to_end(&mut out).ok()?;
        }
        self.total += out.len();
        Some(out)
    }
}

fn filters_supported(dict: &Value) -> Option<bool> {
    // Some(true) = flate, Some(false) = no filter, None = other filters.
    match dict.get(b"Filter") {
        None => Some(false),
        Some(Value::Name(name)) if name == b"FlateDecode" => Some(true),
        Some(Value::Arr(items)) if items.is_empty() => Some(false),
        Some(Value::Arr(items))
            if items.len() == 1
                && matches!(&items[0], Value::Name(name) if name == b"FlateDecode") =>
        {
            Some(true)
        }
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

fn read_objects(data: &[u8], inflater: &mut Inflater) -> HashMap<u32, PdfObject> {
    let mut objects = HashMap::new();
    for (number, body_start) in object_starts(data) {
        let body_end = find(data, b"endobj", body_start).unwrap_or(data.len());
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
        let stream = stream_bytes.and_then(|raw| match filters_supported(&dict) {
            Some(true) => inflater.inflate(raw),
            Some(false) => Some(raw.to_vec()),
            None => None,
        });
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

fn stream_of<'a>(objects: &'a HashMap<u32, PdfObject>, value: &Value) -> Option<&'a [u8]> {
    match value {
        Value::Ref(number) => objects.get(number)?.stream.as_deref(),
        _ => None,
    }
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

fn parse_cmap(data: &[u8]) -> CMap {
    let tokens = tokenize(data, 2_000_000);
    let mut cmap = CMap {
        code_len: 1,
        map: HashMap::new(),
    };
    let mut index = 0;
    while index < tokens.len() && cmap.map.len() < MAX_CMAP_ENTRIES {
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
                    cmap.map.insert(code_of(source), utf16(target));
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
                                cmap.map.insert(code, utf16(&units));
                                if let Some(last) = units.last_mut() {
                                    *last = last.wrapping_add(1);
                                }
                                if cmap.map.len() >= MAX_CMAP_ENTRIES {
                                    break;
                                }
                            }
                            index += 3;
                        }
                        Some(Token::ArrStart) => {
                            let mut code = low;
                            index += 3;
                            while let Some(Token::Str(target)) = tokens.get(index) {
                                cmap.map.insert(code, utf16(target));
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

fn page_fonts(
    objects: &HashMap<u32, PdfObject>,
    page: &Value,
    cmaps: &mut HashMap<u32, CMap>,
) -> HashMap<Vec<u8>, Option<u32>> {
    // Resources may be inherited from ancestors in the page tree.
    let mut node = Some(page);
    let mut fonts = HashMap::new();
    for _ in 0..MAX_DEPTH {
        let Some(current) = node else { break };
        if let Some(resources) = current
            .get(b"Resources")
            .and_then(|value| resolve(objects, value))
        {
            if let Some(Value::Dict(entries)) = resources
                .get(b"Font")
                .and_then(|value| resolve(objects, value))
            {
                for (name, font_ref) in entries {
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
                            cmaps.entry(number).or_insert_with(|| parse_cmap(bytes));
                        }
                    }
                    fonts.entry(name.clone()).or_insert(cmap_ref);
                }
                break;
            }
        }
        node = current
            .get(b"Parent")
            .and_then(|value| resolve(objects, value));
    }
    fonts
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

fn content_text(
    content: &[u8],
    fonts: &HashMap<Vec<u8>, Option<u32>>,
    cmaps: &HashMap<u32, CMap>,
    out: &mut Vec<u8>,
) {
    let mut lexer = Lexer::new(content);
    let mut operands: Vec<Operand> = Vec::new();
    let mut font: Option<&CMap> = None;
    let mut line_y: Option<f64> = None;
    while let Some(token) = lexer.next_token() {
        if out.len() >= MAX_TEXT_BYTES {
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
                    Operand::Name(name) => fonts
                        .get(name)
                        .copied()
                        .flatten()
                        .and_then(|number| cmaps.get(&number)),
                    _ => None,
                });
            }
            b"Tj" | b"'" | b"\"" => {
                if matches!(operator.as_slice(), b"'" | b"\"") {
                    out.push(b'\n');
                }
                if let Some(Operand::Str(bytes)) = operands.last() {
                    out.extend_from_slice(decode_string(bytes, font).as_bytes());
                }
            }
            b"TJ" => {
                if let Some(Operand::Arr(items)) = operands.last() {
                    for item in items {
                        match item {
                            Operand::Str(bytes) => {
                                out.extend_from_slice(decode_string(bytes, font).as_bytes())
                            }
                            // A large negative adjustment is a visual word gap.
                            Operand::Num(value) if *value < -200.0 => out.push(b' '),
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
                    (b"Tm", Some(y)) => line_y.is_some_and(|previous| (previous - y).abs() > 0.5),
                    (_, Some(y)) => y.abs() > 0.5,
                    _ => true,
                };
                if operator.as_slice() == b"Tm" {
                    line_y = y;
                }
                if new_line {
                    out.push(b'\n');
                }
            }
            b"T*" | b"ET" => out.push(b'\n'),
            b"ID" => lexer.skip_inline_image(),
            _ => {}
        }
        operands.clear();
    }
}

fn contents_of(objects: &HashMap<u32, PdfObject>, page: &Value) -> Vec<u8> {
    let mut content = Vec::new();
    match page.get(b"Contents") {
        Some(Value::Arr(parts)) => {
            for part in parts {
                if let Some(bytes) = stream_of(objects, part) {
                    content.extend_from_slice(bytes);
                    content.push(b'\n');
                }
            }
        }
        Some(value) => {
            if let Some(bytes) = stream_of(objects, value) {
                content.extend_from_slice(bytes);
            }
        }
        None => {}
    }
    content
}

/// Decoded streams and recovered page text of a PDF, within fixed limits.
pub(super) fn extract(data: &[u8]) -> PdfContent {
    let mut inflater = Inflater { total: 0 };
    let objects = read_objects(data, &mut inflater);
    let mut cmaps = HashMap::new();
    let mut text = Vec::new();
    let mut pages = objects
        .iter()
        .filter(|(_, object)| object.dict.name_is(b"Type", b"Page"))
        .map(|(number, _)| *number)
        .collect::<Vec<_>>();
    pages.sort_unstable();
    for number in pages.into_iter().take(MAX_PAGES) {
        let page = &objects[&number].dict;
        let fonts = page_fonts(&objects, page, &mut cmaps);
        let content = contents_of(&objects, page);
        content_text(&content, &fonts, &cmaps, &mut text);
        text.push(b'\n');
        if text.len() >= MAX_TEXT_BYTES {
            text.truncate(MAX_TEXT_BYTES);
            break;
        }
    }
    PdfContent {
        streams: objects
            .into_values()
            .filter_map(|object| object.stream)
            .collect(),
        text,
    }
}

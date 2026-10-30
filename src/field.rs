//! Framer for `--array-field NAME`: finds the ONE root-object member whose (unescaped) key equals NAME,
//! hands its array to the M1 `Scanner` (so emission is byte-identical to root mode) and skips every
//! other member. Like `Scanner` it is NOT a validator (serde's `IgnoredAny` checks the whole document);
//! it errors only on what a valid document can still get wrong: root not an object, NAME missing or
//! duplicated, target not an array, depth. State is O(1): keys are compared incrementally against NAME
//! (never buffered) and skipped values are tracked by a nesting counter, so memory does not depend on
//! key, sibling or element size.
use crate::scan::Scanner;

#[derive(Clone, Copy, PartialEq)]
enum St {
    Start,      // before '{'
    ExpectKey,  // after '{' or ',' : '"' (or '}')
    Key,        // inside a root-level key string
    AfterKey,   // ':' expected
    ValueStart, // first byte of a member value
    Skip,       // inside / between skipped values; `depth` = open brackets inside the current value
    Target,     // delegating to the inner Scanner
    Done,       // after the root '}'
}

pub struct FieldScanner {
    st: St,
    name: Vec<u8>,
    max_depth: usize, // nesting limit INCLUDING the root object
    pos: u64,
    depth: usize,
    in_str: bool, // Skip: inside a string
    esc: bool,    // Skip / Key (no longer matching): previous byte was a backslash
    // key comparison against `name`
    ok: bool,     // key so far equals name[..idx]
    idx: usize,
    kesc: u8,     // 0 none, 1 after '\', 2..=5 = 0..=3 hex digits of \uXXXX read
    acc: u32,
    high: u32,    // pending high surrogate (0 = none)
    is_target: bool,
    found: bool,
    inner: Option<Scanner>,
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

impl FieldScanner {
    pub fn new(name: &str, max_depth: usize) -> Self {
        FieldScanner {
            st: St::Start, name: name.as_bytes().to_vec(), max_depth, pos: 0, depth: 0, in_str: false, esc: false,
            ok: true, idx: 0, kesc: 0, acc: 0, high: 0, is_target: false, found: false, inner: None,
        }
    }

    /// Same contract as `Scanner::feed`: lines of the target array are appended to `out`.
    pub fn feed(&mut self, buf: &[u8], out: &mut Vec<u8>) -> Result<(), String> {
        let base = self.pos;
        self.pos += buf.len() as u64;
        let mut i = 0;
        while i < buf.len() {
            let here = base + i as u64;
            match self.st {
                St::Target => {
                    let sc = self.inner.as_mut().expect("target scanner");
                    i += sc.feed_until_done(&buf[i..], out)?;
                    if sc.is_done() {
                        self.inner = None;
                        self.st = St::Skip; // depth 0, not in a string: expects ',' or '}'
                    }
                }
                St::Key if self.ok => {
                    if self.key_byte(buf[i]) { self.end_key()?; }
                    i += 1;
                }
                St::Key | St::Skip if self.in_str || self.st == St::Key => {
                    // no longer interesting string content: only '"' and '\' matter
                    if self.esc {
                        self.esc = false;
                        i += 1;
                        continue;
                    }
                    while i < buf.len() && buf[i] != b'"' && buf[i] != b'\\' { i += 1; }
                    if i == buf.len() { break; }
                    if buf[i] == b'\\' {
                        self.esc = true;
                    } else if self.st == St::Key {
                        self.end_key()?;
                    } else {
                        self.in_str = false;
                    }
                    i += 1;
                }
                _ => {
                    let b = buf[i];
                    match self.st {
                        St::Start if b == b'{' => self.st = St::ExpectKey,
                        St::Start if !is_ws(b) => return Err(format!("root is not an object (byte {here})")),
                        St::ExpectKey if b == b'"' => {
                            (self.ok, self.idx, self.kesc, self.high, self.esc) = (true, 0, 0, 0, false);
                            self.st = St::Key;
                        }
                        St::ExpectKey if b == b'}' => self.st = St::Done,
                        St::ExpectKey if !is_ws(b) => return Err(format!("expected a member name at byte {here}")),
                        St::AfterKey if b == b':' => self.st = St::ValueStart,
                        St::AfterKey if !is_ws(b) => return Err(format!("expected ':' at byte {here}")),
                        St::ValueStart if !is_ws(b) => {
                            if self.is_target {
                                if b != b'[' { return Err(format!("array member {:?} is not an array (byte {here})", self.name_str())); }
                                if self.max_depth < 2 { return Err(self.too_deep(here)); }
                                self.inner = Some(Scanner::embedded(self.max_depth - 1, here));
                                self.st = St::Target;
                                continue; // the inner scanner needs to see the '[' itself
                            } else {
                                self.st = St::Skip;
                                (self.depth, self.in_str, self.esc) = (0, false, false);
                                continue; // reprocess this byte as the start of a skipped value
                            }
                        }
                        St::Skip => match b {
                            b'"' => self.in_str = true,
                            b'{' | b'[' => {
                                // checked per opening bracket; root object = 1, so this one is level depth+2
                                if self.depth + 2 > self.max_depth { return Err(self.too_deep(here)); }
                                self.depth += 1;
                            }
                            b'}' | b']' if self.depth > 0 => self.depth -= 1,
                            b'}' => self.st = St::Done,
                            b']' => return Err(format!("unexpected ']' at byte {here}")),
                            b',' if self.depth == 0 => self.st = St::ExpectKey,
                            _ => {} // scalar bytes, ':' and whitespace: serde judges them
                        },
                        St::Done if !is_ws(b) => return Err(format!("trailing data after root object at byte {here}")),
                        _ => {}
                    }
                    i += 1;
                }
            }
        }
        Ok(())
    }

    fn name_str(&self) -> &str {
        std::str::from_utf8(&self.name).unwrap_or("")
    }

    fn too_deep(&self, here: u64) -> String {
        format!("nesting depth exceeds --max-depth {} at byte {here}", self.max_depth)
    }

    /// Closing quote of a root key reached.
    fn end_key(&mut self) -> Result<(), String> {
        self.is_target = self.ok && self.idx == self.name.len() && self.high == 0 && self.kesc == 0;
        if self.is_target {
            if self.found { return Err(format!("duplicate member {:?}", self.name_str())); }
            self.found = true;
        }
        self.st = St::AfterKey;
        Ok(())
    }

    /// Append `s` (the next decoded bytes of the key) to the comparison.
    fn eat(&mut self, s: &[u8]) {
        if self.high != 0 || self.name.get(self.idx..self.idx + s.len()) != Some(s) {
            self.ok = false; // from here on the key is only lexed (see the `Key` arm of `feed`)
        } else {
            self.idx += s.len();
        }
    }

    /// One byte of a root key while it can still equal NAME. True = the closing quote.
    fn key_byte(&mut self, b: u8) -> bool {
        match self.kesc {
            0 => match b {
                b'"' => return true,
                b'\\' => self.kesc = 1,
                _ => self.eat(&[b]),
            },
            1 => {
                self.kesc = 0;
                match b {
                    b'u' => { self.kesc = 2; self.acc = 0; }
                    b'"' | b'\\' | b'/' => self.eat(&[b]),
                    b'b' => self.eat(b"\x08"),
                    b'f' => self.eat(b"\x0c"),
                    b'n' => self.eat(b"\n"),
                    b'r' => self.eat(b"\r"),
                    b't' => self.eat(b"\t"),
                    _ => self.ok = false, // invalid escape: serde rejects it
                }
            }
            _ => match (b as char).to_digit(16) {
                None => { // invalid \u escape: stop comparing, and re-read this byte as plain string content
                    self.ok = false;
                    self.kesc = 0;
                    return b == b'"' || { self.esc = b == b'\\'; false };
                }
                Some(d) => {
                    self.acc = self.acc * 16 + d;
                    self.kesc += 1;
                    if self.kesc == 6 {
                        self.kesc = 0;
                        self.unit(self.acc);
                    }
                }
            },
        }
        false
    }

    /// A complete \uXXXX escape; surrogates must pair up, a lone one can never equal a valid-UTF-8 NAME.
    fn unit(&mut self, u: u32) {
        match (self.high, u) {
            (0, 0xD800..=0xDBFF) => self.high = u,
            (0, 0xDC00..=0xDFFF) => self.ok = false,
            (0, _) => self.eat_char(u),
            (h, 0xDC00..=0xDFFF) => {
                self.high = 0;
                self.eat_char(0x10000 + ((h - 0xD800) << 10) + (u - 0xDC00));
            }
            _ => self.ok = false,
        }
    }

    fn eat_char(&mut self, c: u32) {
        let mut b = [0u8; 4];
        let s = char::from_u32(c).unwrap_or('\u{fffd}').encode_utf8(&mut b);
        self.eat(s.as_bytes());
    }

    pub fn finish(&self) -> Result<(), String> {
        if self.st != St::Done {
            return Err(format!("unexpected end of input at byte {} (object not closed)", self.pos));
        }
        if !self.found {
            return Err(format!("array member {:?} not found in the root object", self.name_str()));
        }
        Ok(())
    }
}

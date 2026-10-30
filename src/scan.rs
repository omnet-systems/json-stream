//! Byte-oriented framing scanner for ONE top-level JSON array. NOT a JSON validator:
//! it checks only array framing, bracket matching, string/escape termination and
//! separators. Leaf grammar (numbers, literals, escapes' validity) is left to jq.
//! State is O(depth) (the bracket stack); output is produced per fed chunk, so
//! memory does not depend on input or record size.

#[derive(Clone, Copy, PartialEq, Debug)]
enum St {
    Start, // before '['
    First, // after '[' : value or ']'
    Next,  // after ',' : value required
    Value, // inside an element
    After, // element done: ',' or ']'
    Done,  // after final ']'
}

pub struct Scanner {
    st: St,
    stack: Vec<u8>, // expected closers
    in_str: bool,
    esc: bool,
    ws: bool, // whitespace seen inside element, emitted lazily as one ' '
    pos: u64,
    max_depth: usize, // nesting limit INCLUDING the root array
    stop: bool,       // return from feed_until_done right after the closing ']'
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

impl Scanner {
    pub fn new(max_depth: usize) -> Self {
        Scanner { st: St::Start, stack: Vec::new(), in_str: false, esc: false, ws: false, pos: 0, max_depth, stop: false }
    }

    /// Framer for an array embedded in a larger document: `pos` is the global offset of its '[' (error
    /// messages), `max_depth` counts the array as level 1.
    pub fn embedded(max_depth: usize, pos: u64) -> Self {
        Scanner { stop: true, pos, ..Scanner::new(max_depth) }
    }

    pub fn is_done(&self) -> bool {
        self.st == St::Done
    }

    /// Append one chunk's records to `out` (each record + '\n'). `out` grows by at most
    /// chunk.len()+2 bytes. On Err, `out` holds whatever was produced before the fault
    /// (the last line may be an unterminated fragment).
    pub fn feed(&mut self, buf: &[u8], out: &mut Vec<u8>) -> Result<(), String> {
        self.feed_until_done(buf, out).map(drop)
    }

    /// Like `feed`, but for an `embedded` scanner stops right after the closing ']' and returns the
    /// number of bytes consumed (always `buf.len()` otherwise).
    pub fn feed_until_done(&mut self, buf: &[u8], out: &mut Vec<u8>) -> Result<usize, String> {
        let base = self.pos;
        self.pos += buf.len() as u64;
        let mut i = 0;
        while i < buf.len() {
            let at = |i: usize| base + i as u64;
            if self.in_str {
                if self.esc {
                    let b = buf[i];
                    if b == b'\n' || b == b'\r' {
                        return Err(format!("raw line break in string at byte {}", at(i)));
                    }
                    out.push(b);
                    self.esc = false;
                    i += 1;
                    continue;
                }
                let s = i;
                while i < buf.len() && !matches!(buf[i], b'"' | b'\\' | b'\n' | b'\r') {
                    i += 1;
                }
                out.extend_from_slice(&buf[s..i]);
                if i == buf.len() {
                    break;
                }
                let b = buf[i];
                match b {
                    b'"' => {
                        out.push(b);
                        self.in_str = false;
                        if self.stack.is_empty() {
                            self.end_element(out);
                        }
                    }
                    b'\\' => {
                        out.push(b);
                        self.esc = true;
                    }
                    _ => return Err(format!("raw line break in string at byte {}", at(i))),
                }
                i += 1;
                continue;
            }
            let b = buf[i];
            let here = at(i);
            i += 1;
            match self.st {
                St::Start => {
                    if b == b'[' {
                        self.st = St::First;
                    } else if !is_ws(b) {
                        return Err(format!("root is not an array (byte {here})"));
                    }
                }
                St::Done => {
                    if self.stop {
                        i -= 1; // not ours: the byte after the closing ']'
                        break;
                    }
                    if !is_ws(b) {
                        return Err(format!("trailing data after root array at byte {here}"));
                    }
                }
                St::After => match b {
                    b',' => self.st = St::Next,
                    b']' => self.st = St::Done,
                    _ if is_ws(b) => {}
                    _ => return Err(format!("expected ',' or ']' at byte {here}")),
                },
                St::First | St::Next => {
                    if is_ws(b) {
                        continue;
                    }
                    match b {
                        b']' if self.st == St::First => self.st = St::Done,
                        b']' => return Err(format!("trailing comma before ']' at byte {here}")),
                        b',' | b'}' | b':' => return Err(format!("unexpected '{}' at byte {here}", b as char)),
                        _ => {
                            self.st = St::Value;
                            self.ws = false;
                            self.token(b, out, here)?;
                        }
                    }
                }
                St::Value => {
                    if self.stack.is_empty() {
                        // top-level scalar: ends at whitespace, ',' or ']' (never merged across ws)
                        match b {
                            b',' => { self.end_element(out); self.st = St::Next }
                            b']' => { self.end_element(out); self.st = St::Done }
                            _ if is_ws(b) => { self.end_element(out); }
                            b'"' | b'[' | b'{' | b'}' | b':' => {
                                return Err(format!("unexpected '{}' inside scalar at byte {here}", b as char))
                            }
                            _ => out.push(b),
                        }
                    } else if is_ws(b) {
                        self.ws = true;
                    } else {
                        if self.ws {
                            out.push(b' ');
                            self.ws = false;
                        }
                        self.token(b, out, here)?;
                    }
                }
            }
        }
        self.pos = base + i as u64;
        Ok(i)
    }

    // one non-whitespace byte outside strings (start of element or inside container)
    fn token(&mut self, b: u8, out: &mut Vec<u8>, here: u64) -> Result<(), String> {
        out.push(b);
        match b {
            b'"' => self.in_str = true,
            b'{' | b'[' => {
                // checked per opening delimiter (never per chunk); +1 for the root array
                if self.stack.len() + 2 > self.max_depth {
                    return Err(format!("nesting depth exceeds --max-depth {} at byte {here}", self.max_depth));
                }
                self.stack.push(if b == b'{' { b'}' } else { b']' });
            }
            b'}' | b']' => match self.stack.pop() {
                Some(c) if c == b => {
                    if self.stack.is_empty() {
                        self.end_element(out);
                    }
                }
                _ => return Err(format!("mismatched '{}' at byte {here}", b as char)),
            },
            _ => {}
        }
        Ok(())
    }

    fn end_element(&mut self, out: &mut Vec<u8>) {
        out.push(b'\n');
        self.st = St::After;
        self.ws = false;
    }

    pub fn finish(&self) -> Result<(), String> {
        if self.st == St::Done {
            Ok(())
        } else {
            Err(format!("unexpected end of input at byte {} (array not closed)", self.pos))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(input: &[u8], chunk: usize) -> (Vec<u8>, Result<(), String>) {
        let mut sc = Scanner::new(256);
        let mut all = Vec::new();
        for c in input.chunks(chunk.max(1)) {
            let mut out = Vec::new();
            let r = sc.feed(c, &mut out);
            all.extend(out);
            if r.is_err() {
                return (all, r);
            }
        }
        let r = sc.finish();
        (all, r)
    }

    /// Expected output is hand-written; checked at every chunk size (incl. 1-byte feeds,
    /// which split every escape / token / bracket pair).
    fn ok(input: &str, want: &str) {
        for n in 1..=input.len().max(1) + 1 {
            let (got, r) = run(input.as_bytes(), n);
            assert!(r.is_ok(), "{input:?} chunk {n}: {r:?}");
            assert_eq!(String::from_utf8_lossy(&got), want, "{input:?} chunk {n}");
        }
    }

    fn bad(input: &str) {
        for n in 1..=input.len().max(1) + 1 {
            let (_, r) = run(input.as_bytes(), n);
            assert!(r.is_err(), "{input:?} chunk {n} should fail");
        }
    }

    #[test]
    fn basics() {
        ok("[]", "");
        ok(" [ ] \n", "");
        ok("[1,2,3]", "1\n2\n3\n");
        ok("[ 1 , 2 ]", "1\n2\n");
        ok("[{\"a\":1},[2],\"s\",null,true]", "{\"a\":1}\n[2]\n\"s\"\nnull\ntrue\n");
    }

    #[test]
    fn strings_hide_structure() {
        // brackets, commas, escaped quote, escaped backslash right before closing quote
        ok(r#"["]","[{,","a\"b","\\","\\\\",{"k\"}":"}\\"}]"#,
           "\"]\"\n\"[{,\"\n\"a\\\"b\"\n\"\\\\\"\n\"\\\\\\\\\"\n{\"k\\\"}\":\"}\\\\\"}\n");
        ok(r#"["\u0022","\/"]"#, "\"\\u0022\"\n\"\\/\"\n");
    }

    #[test]
    fn nesting() {
        ok("[[[[]]],{},{\"a\":{\"b\":[{}]}}]", "[[[]]]\n{}\n{\"a\":{\"b\":[{}]}}\n");
        // deep nesting: state is a stack, not a recursion
        let d = 100_000;
        let s = format!("[{}{}]", "[".repeat(d), "]".repeat(d));
        let (got, r) = run_with(&mut Scanner::new(usize::MAX), s.as_bytes(), 4096);
        assert!(r.is_ok());
        assert_eq!(got.len(), 2 * d + 1);
    }

    #[test]
    fn numbers_verbatim() {
        ok("[12345678901234567890,1.00000000000000000001,-9223372036854775809,1e400,-0.0,1E+2]",
           "12345678901234567890\n1.00000000000000000001\n-9223372036854775809\n1e400\n-0.0\n1E+2\n");
    }

    #[test]
    fn whitespace_never_merges_tokens() {
        bad("[1 2]"); // must not become valid "12"
        bad("[1\n2]");
        bad("[true false]");
        bad("[\"a\" \"b\"]");
        bad("[{} {}]");
        ok("[[1 2]]", "[1 2]\n"); // inside a value: kept as one space; jq rejects it later
        ok("[[1\r\n\t 2]]", "[1 2]\n");
        ok("[\r\n{\"a\" :\r\n 1}\r\n,\r\n3\r\n]\r\n", "{\"a\" : 1}\n3\n");
    }

    #[test]
    fn crlf_in_string_rejected_but_escapes_kept() {
        bad("[\"a\nb\"]");
        bad("[\"a\rb\"]");
        bad("[\"a\\\nb\"]");
        ok(r#"["x\r\ny"]"#, "\"x\\r\\ny\"\n");
    }

    #[test]
    fn bytes_pass_through() {
        let mut input = b"[\"".to_vec();
        input.extend([0xff, 0xfe, 0x00, 0xc3, 0x28]);
        input.extend(b"\"]");
        let (got, r) = run(&input, 1);
        assert!(r.is_ok());
        assert_eq!(got, [&b"\""[..], &[0xff, 0xfe, 0x00, 0xc3, 0x28], &b"\"\n"[..]].concat());
    }

    #[test]
    fn framing_errors() {
        for s in [
            "", " ", "{\"a\":[1,2]}", "1", "[1,2", "[", "[{\"a\":1}", "[\"abc", "[\"abc\\", "[1,]", "[,1]", "[1,,2]",
            "[1]]", "[1] x", "[1][2]", "[1]\n[2]", "[{]", "[[}]", "[{\"a\":1]", "[[1}]", "[}]", "[1}", "[1\"a\"]", "[1[2]]",
            "[1 ,]", "[:]",
        ] {
            bad(s);
        }
        // framing-only: leaf garbage passes (jq's job), documented limitation
        ok("[{\"a\":}]", "{\"a\":}\n");
        ok("[01,tru]", "01\ntru\n");
    }

    #[test]
    fn depth_is_checked_at_each_opening_delimiter_for_every_chunk_size() {
        let nest = |d: usize| format!("[{}{}]", "[".repeat(d - 1), "]".repeat(d - 1));
        for chunk in [1, 7, 4096, 1 << 20] {
            let mut sc = Scanner::new(3);
            let (_, r) = run_with(&mut sc, nest(3).as_bytes(), chunk);
            assert!(r.is_ok(), "depth 3 chunk {chunk}");
            let mut sc = Scanner::new(3);
            let (_, r) = run_with(&mut sc, nest(4).as_bytes(), chunk);
            assert!(r.unwrap_err().contains("depth"), "depth 4 chunk {chunk}");
            let mut sc = Scanner::new(2); // objects count too
            assert!(run_with(&mut sc, b"[{\"a\":{}}]", chunk).1.is_err());
        }
    }

    fn run_with(sc: &mut Scanner, input: &[u8], chunk: usize) -> (Vec<u8>, Result<(), String>) {
        let mut all = Vec::new();
        for c in input.chunks(chunk) {
            let mut out = Vec::new();
            let r = sc.feed(c, &mut out);
            all.extend(out);
            if r.is_err() { return (all, r); }
        }
        (all, sc.finish())
    }

    #[test]
    fn output_per_chunk_is_bounded() {
        let mut sc = Scanner::new(usize::MAX);
        let mut out = Vec::new();
        let chunk = vec![b'['; 1000];
        sc.feed(b"[", &mut out).unwrap();
        sc.feed(&chunk, &mut out).unwrap();
        assert!(out.len() <= 1000 + 2);
    }
}

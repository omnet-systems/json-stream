//! Validating pass: the framer (scan.rs) emits NDJSON while serde_json's `IgnoredAny` checks the
//! whole-document JSON grammar (iterative, no value built, numbers never converted) over the same
//! bytes. The reader in between also checks UTF-8 incrementally and enforces the depth limit.
use crate::field::FieldScanner;
use crate::scan::Scanner;
use serde::de::{Deserialize, IgnoredAny};
use std::cell::RefCell;
use std::io::{self, BufReader, Read, Write};
use std::rc::Rc;

const CHUNK: usize = 64 * 1024;

pub enum Fail {
    /// Input is not a valid document under the contract (exit 2).
    Data(String),
    /// Reading the input or writing the output failed (exit 74).
    Io(String),
}

struct Utf8 { carry: Vec<u8>, pos: u64 }

impl Utf8 {
    /// Err(offset of the first invalid byte sequence).
    fn feed(&mut self, mut b: &[u8]) -> Result<(), u64> {
        let start = self.pos - self.carry.len() as u64;
        self.pos += b.len() as u64;
        while !self.carry.is_empty() && !b.is_empty() {
            self.carry.push(b[0]);
            b = &b[1..];
            match std::str::from_utf8(&self.carry) {
                Ok(_) => self.carry.clear(),
                Err(e) if e.error_len().is_none() => {}
                Err(_) => return Err(start),
            }
        }
        if let Err(e) = std::str::from_utf8(b) {
            if e.error_len().is_some() {
                return Err(self.pos - b.len() as u64 + e.valid_up_to() as u64);
            }
            self.carry = b[e.valid_up_to()..].to_vec();
        }
        Ok(())
    }
}

/// Root-array framing (default) or `--array-field` framing; both are fed the same bytes.
enum Framer { Root(Scanner), Field(FieldScanner) }

impl Framer {
    fn feed(&mut self, buf: &[u8], out: &mut Vec<u8>) -> Result<(), String> {
        match self { Framer::Root(s) => s.feed(buf, out), Framer::Field(s) => s.feed(buf, out) }
    }
    fn finish(&self) -> Result<(), String> {
        match self { Framer::Root(s) => s.finish(), Framer::Field(s) => s.finish() }
    }
}

struct Tee<R, W> {
    inner: R,
    out: W,
    sc: Framer,
    utf8: Utf8,
    buf: Vec<u8>,
    fail: Rc<RefCell<Option<Fail>>>,
}

impl<R: Read, W: Write> Tee<R, W> {
    fn stop(&self, f: Fail) -> io::Error {
        *self.fail.borrow_mut() = Some(f);
        io::Error::other("stopped")
    }
}

impl<R: Read, W: Write> Read for Tee<R, W> {
    fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
        let n = match self.inner.read(dst) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => return Err(e),
            Err(e) => return Err(self.stop(Fail::Io(format!("read: {e}")))),
        };
        let chunk = &dst[..n];
        self.buf.clear();
        let framed = self.sc.feed(chunk, &mut self.buf);
        // partial output before a fault is written on purpose (streaming semantics)
        if let Err(e) = self.out.write_all(&self.buf) {
            return Err(self.stop(Fail::Io(if e.kind() == io::ErrorKind::BrokenPipe { "output closed early".into() } else { format!("write: {e}") })));
        }
        if let Err(m) = framed { return Err(self.stop(Fail::Data(m))); }
        if let Err(at) = self.utf8.feed(chunk) { return Err(self.stop(Fail::Data(format!("invalid UTF-8 at byte {at}")))); }
        Ok(n)
    }
}

/// Validate `input` as one UTF-8 JSON array nested at most `max_depth` deep (root counts as 1),
/// writing one raw element per line to `out`. `Ok` only if the whole input was valid and read to EOF.
/// Output may have been written (up to ~one read-ahead ahead of the fault) even when this fails.
/// With `Some(name)` the root must be an object and the elements of its ONE member
/// `name` (an array) are emitted; the root object counts as depth 1 and every other member is only
/// grammar-checked and skipped.
pub fn validate_with<R: Read, W: Write>(input: R, out: W, max_depth: usize, field: Option<&str>) -> Result<W, Fail> {
    let sc = match field { None => Framer::Root(Scanner::new(max_depth)), Some(n) => Framer::Field(FieldScanner::new(n, max_depth)) };
    let fail = Rc::new(RefCell::new(None));
    let tee = Tee { inner: input, out, sc, utf8: Utf8 { carry: Vec::new(), pos: 0 }, buf: Vec::with_capacity(2 * CHUNK + 16), fail: fail.clone() };
    let mut rd = BufReader::with_capacity(CHUNK, tee);
    let res = {
        let mut de = serde_json::Deserializer::from_reader(&mut rd);
        IgnoredAny::deserialize(&mut de).and_then(|_| de.end())
    };
    let tee = rd.into_inner();
    // a stashed framer/UTF-8/IO failure is more specific than serde's wrapped io error
    if let Some(f) = fail.borrow_mut().take() {
        // serde syntax errors precede read-ahead faults in the stream, so they win
        return Err(match (res, f) {
            (Err(e), Fail::Data(_)) if !e.is_io() => Fail::Data(e.to_string()),
            (_, f) => f,
        });
    }
    // serde's errors carry a trustworthy line/column; framer offsets are global bytes
    if let Err(e) = res {
        return Err(if e.is_io() { Fail::Io(format!("read: {e}")) } else { Fail::Data(e.to_string()) });
    }
    if let Err(m) = tee.sc.finish() { return Err(Fail::Data(m)); }
    if !tee.utf8.carry.is_empty() { return Err(Fail::Data("invalid UTF-8: truncated sequence at end of input".into())); }
    Ok(tee.out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Yields at most `n` bytes per read so every multi-byte sequence / token can be split anywhere.
    struct Drip<'a>(&'a [u8], usize);
    impl Read for Drip<'_> {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            let k = self.1.min(self.0.len()).min(b.len());
            b[..k].copy_from_slice(&self.0[..k]);
            self.0 = &self.0[k..];
            Ok(k)
        }
    }

    fn go(input: &[u8], n: usize) -> Result<Vec<u8>, bool> {
        validate_with(Drip(input, n), Vec::new(), 256, None).map_err(|f| matches!(f, Fail::Data(_)))
    }

    #[test]
    fn multibyte_utf8_split_at_every_boundary_is_accepted() {
        let doc = "[\"é€😀\",\"a\"]".as_bytes();
        for n in 1..=doc.len() + 1 {
            assert_eq!(go(doc, n).unwrap(), "\"é€😀\"\n\"a\"\n".as_bytes(), "chunk {n}");
        }
    }

    #[test]
    fn invalid_utf8_is_rejected_at_every_boundary() {
        let cases: &[&[u8]] = &[b"[\"\xe2\x28\xa1\"]", b"[\"\xc3\"]", b"[\"\xf0\x9f\x98\"]", b"[\"\xed\xa0\x80\"]", b"[\"a\xe2\x82"];
        for c in cases {
            for n in 1..=c.len() + 1 {
                assert_eq!(go(c, n).err(), Some(true), "{c:?} chunk {n}");
            }
        }
    }

    #[test]
    fn utf8_error_offset_is_exact() {
        let mut u = Utf8 { carry: vec![], pos: 0 };
        u.feed(b"[\"ab").unwrap();
        assert_eq!(u.feed(b"\xe2\x82"), Ok(()));
        assert_eq!(u.feed(b"\x28"), Err(4)); // sequence started at byte 4
    }
}

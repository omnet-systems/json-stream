//! Chunk-boundary tests for the `--array-field` framer: the real validate() pipeline is fed through a
//! reader that yields at most `n` bytes per read, for EVERY n, so every escape, surrogate pair, key,
//! bracket and UTF-8 sequence is split at every possible position.
#![allow(dead_code)]
#[path = "../src/field.rs"]
mod field;
#[path = "../src/scan.rs"]
mod scan;
#[path = "../src/validate.rs"]
mod validate;

use std::io::{self, Read};
use validate::{validate_with, Fail};

struct Drip<'a>(&'a [u8], usize);
impl Read for Drip<'_> {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        let k = self.1.min(self.0.len()).min(b.len());
        b[..k].copy_from_slice(&self.0[..k]);
        self.0 = &self.0[k..];
        Ok(k)
    }
}

/// Ok(output) or Err(true) for invalid data / Err(false) for I/O.
fn go(name: &str, input: &[u8], n: usize, depth: usize) -> Result<Vec<u8>, bool> {
    validate_with(Drip(input, n), Vec::new(), depth, Some(name)).map_err(|f| matches!(f, Fail::Data(_)))
}

fn ok(name: &str, input: &str, want: &str) {
    for n in 1..=input.len() + 1 {
        assert_eq!(go(name, input.as_bytes(), n, 256).as_deref().map(String::from_utf8_lossy).as_deref(), Ok(want), "{input:?} chunk {n}");
    }
}

fn bad(name: &str, input: &[u8]) {
    for n in 1..=input.len() + 1 {
        assert_eq!(go(name, input, n, 256).err(), Some(true), "{:?} chunk {n}", String::from_utf8_lossy(input));
    }
}

#[test]
fn escaped_and_surrogate_pair_keys_at_every_chunk_size() {
    ok("features", r#"{"fea\u0074ures":[1,{"a":"b"}]}"#, "1\n{\"a\":\"b\"}\n");
    ok("features", r#"{"\u0066\u0065\u0061\u0074\u0075\u0072\u0065\u0073":[1]}"#, "1\n");
    ok("😀", r#"{"\ud83d\ude00":[1]}"#, "1\n");
    ok("😀", r#"{"x":0,"😀":[1],"\uD83D\uDE00y":2}"#, "1\n");
    ok("é€😀", r#"{"\u00e9€\ud83d\ude00":[1]}"#, "1\n");
    ok("a\"b\\c/\n", r#"{"a\"b\\c\/\n":[1]}"#, "1\n");
    bad("😀", br#"{"\ud83d\ude00":[1],"\ud83d\ude00":[]}"#);
    bad("features", br#"{"features":[],"featur\u0065s":[]}"#);
    bad("😀", br#"{"\ud83d\ude01":[1]}"#);
    bad("😀", br#"{"\ud83d":[1]}"#);
    bad("😀", br#"{"\ud83d\u0041":[1]}"#);
    bad("😀", br#"{"\ude00\ud83d":[1]}"#);
    bad("features", br#"{"featuresx":[1]}"#);
    bad("features", br#"{"feature":[1]}"#);
}

#[test]
fn skipped_lone_surrogate_keys_and_structure_hiding_strings() {
    ok("f", r#"{"\ud800":1,"\udc00":2,"\ud800\u0041":3,"\ud800\\":4,"f":[9]}"#, "9\n");
    ok("f", r#"{"a":"}]\"}","b":["\\","x"],"f":[1],"c":{"k\"}":"\\"}}"#, "1\n");
    ok("f", r#"{"a\"f":[0],"f":["f\"]"]}"#, "\"f\\\"]\"\n");
}

#[test]
fn elements_and_trailer_at_every_chunk_size() {
    ok("f", r#"{ "f" : [ 1e400 , "\ud800" , {"a" :1,"a":2}, [ 1 , 2 ] ] , "m" : { "z" : [ ] } }"#, "1e400\n\"\\ud800\"\n{\"a\" :1,\"a\":2}\n[ 1 , 2 ]\n");
    ok("f", "{\"m\":1,\"f\":[],\"n\":\"é€😀\"}\r\n", "");
    bad("f", br#"{"f":[1],"m":{"a":}}"#);
    bad("f", br#"{"f":[1]}x"#);
    bad("f", br#"{"f":[1],"m":"abc"#);
    bad("f", b"{\"f\":[1],\"m\":\"\xc3\xa9\xe2\x82\xac\xf0\x9f\x98\x80\xff\"}");
    bad("f", b"{\"f\":[1],\"m\":\"\xf0\x9f\x98");
}

#[test]
fn depth_is_checked_at_each_opening_bracket_for_every_chunk_size() {
    let nest = |d: usize| format!("{}{}", "[".repeat(d), "]".repeat(d));
    for n in [1, 7, 4096, 1 << 20] {
        // root object 1, target 2, element containers 3.. ; sibling nesting 2..
        let tgt = format!(r#"{{"f":[{}]}}"#, nest(3));
        assert!(go("f", tgt.as_bytes(), n, 5).is_ok(), "chunk {n}");
        assert_eq!(go("f", tgt.as_bytes(), n, 4).err(), Some(true), "chunk {n}");
        let sib = format!(r#"{{"s":{},"f":[]}}"#, nest(3));
        assert!(go("f", sib.as_bytes(), n, 4).is_ok(), "chunk {n}");
        assert_eq!(go("f", sib.as_bytes(), n, 3).err(), Some(true), "chunk {n}");
        let after = format!(r#"{{"f":[],"s":{}}}"#, nest(3));
        assert!(go("f", after.as_bytes(), n, 4).is_ok(), "chunk {n}");
        assert_eq!(go("f", after.as_bytes(), n, 3).err(), Some(true), "chunk {n}");
        assert!(go("f", b"{\"f\":[]}", n, 2).is_ok());
        assert_eq!(go("f", b"{\"f\":[]}", n, 1).err(), Some(true));
    }
}

#[test]
fn output_is_identical_for_every_chunk_size() {
    let doc = r#"{"a":[0],"f\u0065":[1,"two",{"k":[3.50,-0.0]}],"z":{"f":[9]}}"#;
    let want = go("fe", doc.as_bytes(), 1 << 20, 256).unwrap();
    assert_eq!(want, b"1\n\"two\"\n{\"k\":[3.50,-0.0]}\n");
    for n in 1..=doc.len() {
        assert_eq!(go("fe", doc.as_bytes(), n, 256).unwrap(), want, "chunk {n}");
    }
}

struct Failing<'a>(&'a [u8], usize);
impl Read for Failing<'_> {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        if self.0.len() <= self.1 { return Err(io::Error::other("boom")); }
        let k = 3.min(self.0.len() - self.1).min(b.len());
        b[..k].copy_from_slice(&self.0[..k]);
        self.0 = &self.0[k..];
        Ok(k)
    }
}

#[test]
fn read_errors_are_io_not_data() {
    let doc = br#"{"f":[1,2,3],"m":{"a":1}}"#;
    for tail in 0..doc.len() {
        let r = validate_with(Failing(&doc[..], tail), Vec::new(), 256, Some("f"));
        assert!(matches!(r, Err(Fail::Io(_))), "fail after {} bytes", doc.len() - tail);
    }
}

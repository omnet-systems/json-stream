//! CLI tests against the real binary; jq (installed) is the independent oracle.
use std::io::Write;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_json-stream");

fn run_stdin(input: &[u8]) -> (Vec<u8>, i32) {
    let mut c = Command::new(BIN).arg("-").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut sin = c.stdin.take().unwrap();
    let inp = input.to_vec();
    let t = std::thread::spawn(move || { let _ = sin.write_all(&inp); });
    let o = c.wait_with_output().unwrap();
    t.join().unwrap();
    (o.stdout, o.status.code().unwrap_or(-1))
}

fn jq(args: &[&str], input: &[u8]) -> Option<Vec<u8>> {
    let mut c = Command::new("jq").args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut sin = c.stdin.take().unwrap();
    let inp = input.to_vec();
    let t = std::thread::spawn(move || { let _ = sin.write_all(&inp); });
    let o = c.wait_with_output().ok()?;
    t.join().unwrap();
    Some(o.stdout)
}

#[test]
fn fixtures_exit_codes() {
    for (f, want_ok) in [
        ("fixtures/empty.json", true), ("fixtures/escaped.json", true), ("fixtures/hetero.json", true),
        ("fixtures/bad_mid.json", false), // leaf grammar is now validated (M1)
        ("fixtures-extra/bigint_exact.json", true), ("fixtures-extra/crlf.json", true),
        ("fixtures-extra/dup_key.json", true), ("fixtures-extra/invalid_utf8.json", false),
        ("fixtures/root_object.json", false), ("fixtures/trail_garbage.json", false),
        ("fixtures/trailing_comma.json", false), ("fixtures/truncated.json", false), ("fixtures/two_docs.json", false),
    ] {
        let code = Command::new(BIN).arg(f).stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap().code().unwrap();
        assert_eq!(code == 0, want_ok, "{f} exit {code}");
    }
}

#[test]
fn output_is_one_line_per_record_and_lossless() {
    let o = Command::new(BIN).arg("fixtures-extra/crlf.json").output().unwrap();
    assert_eq!(o.stdout, b"{\"a\":1}\n{\"b\":\"x\\r\\ny\"}\n3\n");
    let o = Command::new(BIN).arg("fixtures-extra/dup_key.json").output().unwrap();
    assert_eq!(o.stdout, b"{\"a\":1,\"a\":2}\n{\"k\":\"v\",\"k\":\"w\",\"k\":null}\n");
    let o = Command::new(BIN).arg("fixtures-extra/bigint_exact.json").output().unwrap();
    assert_eq!(&o.stdout[..], &b"{\"n\":9007199254740993}\n12345678901234567890\n1.00000000000000000001\n-9223372036854775809\n"[..]);
}

#[test]
fn partial_stdout_then_nonzero() {
    let (out, code) = run_stdin(b"[{\"a\":1},{\"a\":2},]");
    assert_eq!(out, b"{\"a\":1}\n{\"a\":2}\n");
    assert_ne!(code, 0);
}

#[test]
fn stdin_equals_file() {
    let f = std::fs::read("fixtures/hetero.json").unwrap();
    let (a, c) = run_stdin(&f);
    assert_eq!(c, 0);
    assert_eq!(a, Command::new(BIN).arg("fixtures/hetero.json").output().unwrap().stdout);
}

#[test]
fn missing_file_and_bad_usage() {
    assert_ne!(Command::new(BIN).arg("nope.json").stderr(Stdio::null()).status().unwrap().code(), Some(0));
    assert_ne!(Command::new(BIN).stderr(Stdio::null()).status().unwrap().code(), Some(0));
}

// tiny deterministic generator of adversarial values with random inter-token whitespace
struct R(u64);
impl R {
    fn n(&mut self, m: u64) -> u64 { self.0 ^= self.0 << 13; self.0 ^= self.0 >> 7; self.0 ^= self.0 << 17; self.0 % m }
    fn ws(&mut self) -> &'static str { ["", "", " ", "\n", "\r\n", "\t", " \r\n "][self.n(7) as usize] }
    fn string(&mut self) -> String {
        let pieces = ["a", "\\\"", "\\\\", "\\\\\\\"", "]", "[", "{", "}", ",", ":", "\\n", "\\u0022", "\\/", "é", "😀", " ", "\\\\\\\\"];
        let mut s = String::from("\"");
        for _ in 0..self.n(12) { s += pieces[self.n(pieces.len() as u64) as usize]; }
        s + "\""
    }
    fn num(&mut self) -> String {
        ["0", "-1", "12345678901234567890", "1.00000000000000000001", "1e400", "-0.0", "1E+2", "9007199254740993"][self.n(8) as usize].into()
    }
    fn value(&mut self, d: u32) -> String {
        match self.n(if d > 4 { 5 } else { 8 }) {
            0 => self.string(), 1 => self.num(), 2 => "null".into(), 3 => "true".into(), 4 => "false".into(),
            5 | 6 => {
                let mut s = String::from("[") + self.ws();
                for i in 0..self.n(4) { if i > 0 { s += &format!("{},{}", self.ws(), self.ws()); } s += &self.value(d + 1); }
                s + self.ws() + "]"
            }
            _ => {
                let mut s = String::from("{") + self.ws();
                for i in 0..self.n(4) {
                    if i > 0 { s += &format!("{},{}", self.ws(), self.ws()); }
                    s += &format!("{}{}:{}{}", self.string(), self.ws(), self.ws(), self.value(d + 1));
                }
                s + self.ws() + "}"
            }
        }
    }
}

#[test]
fn random_arrays_match_jq() {
    if jq(&["--version"], b"").is_none() { eprintln!("jq missing; skipped"); return; }
    for seed in 1..=20u64 {
        let mut r = R(seed.wrapping_mul(0x9E3779B97F4A7C15));
        let mut doc = String::from("[") + r.ws();
        for i in 0..500 { if i > 0 { doc += &format!("{},{}", r.ws(), r.ws()); } doc += &r.value(0); }
        doc += r.ws();
        doc += "]";
        doc += r.ws();
        let want = jq(&["-c", ".[]"], doc.as_bytes()).unwrap();
        assert!(!want.is_empty(), "seed {seed}: jq rejected the generated doc");
        let (mine, code) = run_stdin(doc.as_bytes());
        assert_eq!(code, 0, "seed {seed}");
        assert!(mine.iter().filter(|&&b| b == b'\n').count() == 500, "seed {seed}: line count");
        let got = jq(&["-c", "."], &mine).unwrap();
        assert_eq!(String::from_utf8_lossy(&got), String::from_utf8_lossy(&want), "seed {seed}");
    }
}

//! M2 `--array-field NAME` contract, tested through the real binary. Expectations are hand-written
//! literals; the independent oracle for the differential is Python's json module (tests/oracle_field.py).
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_json-stream");

struct Out { stdout: Vec<u8>, stderr: String, code: i32 }

fn scratch(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn run(args: &[&str], stdin: &[u8]) -> Out {
    let mut c = Command::new(BIN).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut sin = c.stdin.take().unwrap();
    let inp = stdin.to_vec();
    let t = std::thread::spawn(move || { let _ = sin.write_all(&inp); });
    let o = c.wait_with_output().unwrap();
    t.join().unwrap();
    Out { stdout: o.stdout, stderr: String::from_utf8_lossy(&o.stderr).into(), code: o.status.code().unwrap_or(-1) }
}

fn field(name: &str, input: &str) -> Out { run(&["--array-field", name, "-"], input.as_bytes()) }
fn field_b(name: &str, input: &[u8]) -> Out { run(&["--array-field", name, "-"], input) }

fn ok(name: &str, input: &str, want: &str) {
    let o = field(name, input);
    assert_eq!(o.code, 0, "{input:?}: {}", o.stderr);
    assert_eq!(String::from_utf8(o.stdout).unwrap(), want, "{input:?}");
}

fn bad(name: &str, input: &str) {
    let o = field(name, input);
    assert_eq!(o.code, 2, "{input:?} should be invalid data: {}", o.stderr);
    assert!(!o.stderr.is_empty());
}

// ---- 1, 2: basics and lexeme fidelity ----

#[test]
fn extracts_named_member_of_root_object() {
    ok("features", r#"{"type":"FeatureCollection","features":[{"a":1},2,"s"],"meta":{"n":1}}"#, "{\"a\":1}\n2\n\"s\"\n");
    // the same file without the flag is a non-array root
    assert_eq!(run(&["-"], br#"{"features":[1]}"#).code, 2);
}

#[test]
fn elements_keep_lexemes_and_normalise_whitespace_like_root_mode() {
    let big = "9".repeat(300);
    let doc = format!("{{ \"x\" : 1 ,\r\n \"f\" : [ 1e400 , -12345678901234567890123,{big} ,-0.0,\"\\ud800\",\"\\u00e9\",{{\"a\":1,\"a\":2}},\r\n[ 1 ,\t{{ }} ]\r\n] }}\n");
    let want = format!("1e400\n-12345678901234567890123\n{big}\n-0.0\n\"\\ud800\"\n\"\\u00e9\"\n{{\"a\":1,\"a\":2}}\n[ 1 , {{ }} ]\n");
    let f = field("f", &doc);
    assert_eq!(f.code, 0, "{}", f.stderr);
    assert_eq!(String::from_utf8(f.stdout).unwrap(), want);
    // identical bytes to root mode on the bare array
    let arr = &doc[doc.find('[').unwrap()..doc.rfind(']').unwrap() + 1];
    let r = run(&["-"], arr.as_bytes());
    assert_eq!(r.code, 0);
    assert_eq!(String::from_utf8(r.stdout).unwrap(), want);
}

// ---- 4: escaped-equivalent keys, surrogate pairs ----

#[test]
fn escaped_key_spellings_match() {
    for k in [r#""features""#, r#""fea\u0074ures""#, r#""\u0066eatures""#, r#""f\u0065\u0061tures""#, r#""featur\u0065s""#, r#""\u0066\u0065\u0061\u0074\u0075\u0072\u0065\u0073""#] {
        ok("features", &format!("{{{k}:[1]}}"), "1\n");
    }
    ok("a/b\"c\\d\u{8}\u{c}\n\r\t", r#"{"a\/b\"c\\d\b\f\n\r\t":[7]}"#, "7\n");
    ok("A", r#"{"\u0041":[7]}"#, "7\n"); // upper/lower hex digits
    ok("\u{ab}", r#"{"\u00AB":[7]}"#, "7\n");
}

#[test]
fn near_misses_do_not_match() {
    for k in [r#""feature""#, r#""featuress""#, r#""Features""#, r#""featur\u0065""#, r#""features\u0000""#, r#""\u0000features""#, r#""fea\u0074ure""#, r#""""#, r#""featurez""#] {
        bad("features", &format!("{{{k}:[1]}}"));
    }
}

#[test]
fn surrogate_pair_and_raw_utf8_names_match_each_other() {
    for k in [r#""😀""#, r#""\ud83d\ude00""#, r#""\uD83D\uDE00""#, r#""\ud83d\uDE00""#] {
        ok("😀", &format!("{{{k}:[1]}}"), "1\n");
    }
    for k in [r#""é€""#, r#""\u00e9\u20ac""#, r#""\u00E9€""#, r#""é\u20AC""#] {
        ok("é€", &format!("{{{k}:[2]}}"), "2\n");
    }
    bad("😀", r#"{"\ud83d\ude01":[1]}"#);
    bad("😀", r#"{"\ud83d":[1]}"#);
    bad("😀", r#"{"\ude00":[1]}"#);
    bad("😀", r#"{"\ud83d\ud83d":[1]}"#);
}

// ---- 5: lone-surrogate keys are grammatical but never match ----

#[test]
fn lone_surrogate_keys_are_skipped_and_never_match() {
    ok("features", r#"{"\ud800":1,"features":[]}"#, "");
    ok("features", r#"{"\udc00x":1,"\ud800\u0041":2,"\ud800\\u0041":3,"features":[5]}"#, "5\n");
    for n in ["\u{FFFD}", "\u{10000}", "\u{FFFF}", "", "x"] {
        bad(n, r#"{"\ud800":[1]}"#);
        bad(n, r#"{"\udc00":[1]}"#);
        bad(n, r#"{"\ud800\u0041":[1]}"#);
        bad(n, r#"{"\ud800x":[1]}"#);
        bad(n, r#"{"\ud800\n":[1]}"#);
    }
}

#[test]
fn empty_name_is_a_legal_key() {
    ok("", r#"{"":[1,2],"a":3}"#, "1\n2\n");
    assert_eq!(run(&["--array-field=", "-"], br#"{"":[9]}"#).stdout, b"9\n");
    bad("", r#"{"a":[1]}"#);
}

// ---- 6: target errors ----

#[test]
fn missing_duplicate_and_wrongly_typed_targets_exit_2() {
    bad("features", r#"{}"#);
    bad("features", r#"{"a":[1]}"#);
    bad("features", r#"{"a":{"features":[1]}}"#); // nested name is not a root member
    bad("features", r#"{"a":[{"features":[1]}]}"#);
    bad("features", r#"{"features":[],"features":[]}"#);
    bad("features", r#"{"features":[1],"featur\u0065s":[2]}"#);
    bad("features", r#"{"f\u0065atures":[],"fea\u0074ures":[]}"#);
    bad("features", r#"{"features":[],"x":1,"features":null}"#);
    for v in ["null", "{}", "1", "\"s\"", "true", "{\"a\":[1]}", "1e400"] {
        bad("features", &format!("{{\"features\":{v}}}"));
    }
    for root in ["[]", "[{\"features\":[1]}]", "1", "\"features\"", "null", "", "  "] {
        bad("features", root);
    }
}

#[test]
fn duplicate_target_is_reported_as_such() {
    let o = field("f", r#"{"f":[1,2],"f":[3]}"#);
    assert_eq!(o.code, 2);
    assert!(o.stderr.contains("duplicate"), "{}", o.stderr);
}

// ---- 7: whole-document validation, including metadata after the array ----

#[test]
fn malformed_trailer_after_the_array_is_rejected() {
    for s in [
        r#"{"f":[1],"meta":{"a":}}"#, r#"{"f":[1],"m":1,}"#, r#"{"f":[1],"m":01}"#, r#"{"f":[1],"m":"\x"}"#, r#"{"f":[1],"m":"abc}"#,
        r#"{"f":[1],"m":1"#, r#"{"f":[1]"#, r#"{"f":[1]}x"#, r#"{"f":[1]}{"f":[1]}"#, r#"{"f":[1]} }"#, r#"{"f":[1] "m":2}"#, r#"{"f":[1],"m"}"#,
        r#"{"f":[1],"m":[1,]}"#, r#"{"f":[1],"m":tru}"#, r#"{"f":[1],"m":{"a" 1}}"#, r#"{"f":[1],"m":[1}}"#, r#"{"f":[1],"m":NaN}"#,
        r#"{"f":[1],"m":"a	b"}"#, r#"{"f":[1],3:4}"#, r#"{"f":[1],"m":1e}"#,
    ] {
        bad("f", s);
    }
    // ... and in the prefix before the array
    for s in [r#"{"m":{"a":},"f":[1]}"#, r#"{"m":01,"f":[1]}"#, r#"{"m":"\q","f":[1]}"#, r#"{,"f":[1]}"#, r#"{"m" 1,"f":[1]}"#] {
        bad("f", s);
    }
    // grammar errors inside elements are still caught by serde
    for s in [r#"{"f":[{"a":}]}"#, r#"{"f":[01]}"#, r#"{"f":[1,]}"#, r#"{"f":[1 2]}"#] {
        bad("f", s);
    }
}

#[test]
fn invalid_utf8_anywhere_is_rejected() {
    let mut v = br#"{"f":[1],"m":""#.to_vec();
    v.extend([0xff]);
    v.extend(br#""}"#);
    assert_eq!(field_b("f", &v).code, 2);
    assert_eq!(field_b("f", b"{\"m\":\"\xc3\",\"f\":[1]}").code, 2);
    assert_eq!(field_b("f", b"{\"f\":[\"\xe2\x82\"]}").code, 2);
    assert_eq!(field_b("f", b"{\"\xed\xa0\x80\":1,\"f\":[]}").code, 2);
    assert_eq!(field_b("f", b"\xef\xbb\xbf{\"f\":[]}").code, 2);
    assert_eq!(field_b("f", b"{\"f\":[1]}\xff").code, 2);
    assert_eq!(field_b("f", b"{\"f\":[1],\"m\":\"\xe2\x82").code, 2);
}

#[test]
fn output_file_is_published_only_for_valid_field_documents() {
    let d = scratch("field_out");
    let leftovers = |d: &PathBuf| fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect::<Vec<_>>();
    let good = d.join("good.ndjson");
    let o = run(&["--array-field", "f", "--output", good.to_str().unwrap(), "-"], br#"{"f":[1,{"a":2}],"m":0}"#);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(fs::read(&good).unwrap(), b"1\n{\"a\":2}\n");
    for (i, doc) in [r#"{"f":[1],"m":{"a":}}"#, r#"{"g":[1]}"#, r#"{"f":[1],"f":[1]}"#, r#"{"f":1}"#].iter().enumerate() {
        let p = d.join(format!("bad{i}.ndjson"));
        let o = run(&["--array-field", "f", "--output", p.to_str().unwrap(), "-"], doc.as_bytes());
        assert_eq!(o.code, 2, "{doc}");
        assert!(!p.exists(), "{doc}");
    }
    assert_eq!(leftovers(&d), vec!["good.ndjson".to_string()], "no temp files left");
    // no-clobber still holds
    let o = run(&["--array-field", "f", "--output", good.to_str().unwrap(), "-"], br#"{"f":[9]}"#);
    assert_eq!(o.code, 73);
    assert_eq!(fs::read(&good).unwrap(), b"1\n{\"a\":2}\n");
}

// ---- 8: depth ----

fn nest(d: usize) -> String { format!("{}{}", "[".repeat(d), "]".repeat(d)) }

#[test]
fn depth_counts_root_object_and_ignored_siblings() {
    // root object = 1, target array = 2, element containers 3+
    let tgt = |inner: usize| format!(r#"{{"f":[{}]}}"#, if inner == 0 { String::new() } else { nest(inner) });
    assert_eq!(run(&["--array-field", "f", "--max-depth", "2", "-"], tgt(0).as_bytes()).code, 0);
    assert_eq!(run(&["--array-field", "f", "--max-depth", "1", "-"], tgt(0).as_bytes()).code, 2);
    assert_eq!(run(&["--array-field", "f", "--max-depth", "5", "-"], tgt(3).as_bytes()).code, 0); // 1+1+3
    assert_eq!(run(&["--array-field", "f", "--max-depth", "4", "-"], tgt(3).as_bytes()).code, 2);
    // ignored sibling: object(1) + sibling nesting
    let sib = |inner: usize| format!(r#"{{"s":{},"f":[]}}"#, nest(inner));
    let sib_after = |inner: usize| format!(r#"{{"f":[],"s":{}}}"#, nest(inner));
    for mk in [&sib as &dyn Fn(usize) -> String, &sib_after] {
        assert_eq!(run(&["--array-field", "f", "--max-depth", "4", "-"], mk(3).as_bytes()).code, 0);
        let o = run(&["--array-field", "f", "--max-depth", "3", "-"], mk(3).as_bytes());
        assert_eq!(o.code, 2);
        assert!(o.stderr.contains("depth"), "{}", o.stderr);
    }
    // default 256: 1 + 255 passes, 1 + 256 fails
    assert_eq!(field("f", &sib(255)).code, 0);
    assert_eq!(field("f", &sib(256)).code, 2);
    assert_eq!(field("f", &format!(r#"{{"f":[{}]}}"#, nest(254))).code, 0);
    assert_eq!(field("f", &format!(r#"{{"f":[{}]}}"#, nest(255))).code, 2);
}

#[test]
fn objects_inside_ignored_siblings_count_too() {
    assert_eq!(run(&["--array-field", "f", "--max-depth", "2", "-"], br#"{"s":{"a":{"b":1}},"f":[]}"#).code, 2);
    assert_eq!(run(&["--array-field", "f", "--max-depth", "3", "-"], br#"{"s":{"a":{"b":1}},"f":[]}"#).code, 0);
}

fn vm_limited(limit_kib: u64, args: &str) -> Command {
    let mut c = Command::new("sh");
    c.arg("-c").arg(format!("ulimit -v {limit_kib}; exec \"$0\" {args}")).arg(BIN);
    c
}

/// Streams `parts` (each repeated) into a ulimit-capped process, returns (exit code, stdout byte count, stderr).
fn stream(args: &str, parts: Vec<(Vec<u8>, usize)>) -> (i32, u64, String) {
    let mut c = vm_limited(150_000, args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut sin = c.stdin.take().unwrap();
    let mut sout = c.stdout.take().unwrap();
    let t = std::thread::spawn(move || {
        for (p, n) in parts { for _ in 0..n { if sin.write_all(&p).is_err() { return; } } }
    });
    let mut n = 0u64;
    let mut buf = vec![0u8; 1 << 20];
    loop { let k = sout.read(&mut buf).unwrap(); if k == 0 { break; } n += k as u64; }
    t.join().unwrap();
    let o = c.wait_with_output().unwrap();
    (o.status.code().unwrap_or(-1), n, String::from_utf8_lossy(&o.stderr).into())
}

fn v(s: &str) -> Vec<u8> { s.as_bytes().to_vec() }

#[test]
fn deep_ignored_sibling_fails_cleanly_and_deep_allowed_one_passes_without_recursion() {
    let n = 200_000usize;
    // allowed, before the target and after it
    let (code, out, err) = stream(&format!("--array-field f --max-depth {} -", n + 5), vec![(v("{\"s\":"), 1), (v("["), n), (v("]"), n), (v(",\"f\":[1],\"t\":"), 1), (v("{\"a\":["), n / 2), (v("]}"), n / 2), (v("}"), 1)]);
    assert_eq!((code, out), (0, 2), "{err}");
    // absurd, default depth: stops at the cap
    let (code, _, err) = stream("--array-field f -", vec![(v("{\"s\":"), 1), (v("["), 16 << 20)]);
    assert_eq!(code, 2);
    assert!(err.contains("depth"), "{err}");
}

// ---- 3: gigantic keys / siblings / elements stay streaming ----

const MIB: usize = 1 << 20;

#[test]
fn gigantic_string_element_streams() {
    let (code, out, err) = stream("--array-field f -", vec![(v("{\"f\":[\""), 1), (vec![b'a'; MIB], 201), (v("\",1]}"), 1)]);
    assert_eq!((code, out), (0, (201 * MIB + 2 + 1 + 2) as u64), "{err}");
}

#[test]
fn gigantic_unmatched_sibling_string_array_object_before_and_after() {
    let sib = |arr: bool| -> Vec<(Vec<u8>, usize)> {
        if arr { vec![(v("["), 1), (v("0,"), 100 * MIB), (v("0]"), 1)] } else { vec![(v("\""), 1), (vec![b'z'; MIB], 201), (v("\""), 1)] }
    };
    for kind in [false, true] {
        let mut parts = vec![(v("{\"a\":"), 1)];
        parts.extend(sib(kind));
        parts.push((v(",\"f\":[1,2],\"b\":"), 1));
        parts.extend(sib(kind));
        parts.push((v(",\"c\":{\"k\":"), 1));
        parts.extend(sib(kind));
        parts.push((v("}}"), 1));
        let (code, out, err) = stream("--array-field f -", parts);
        assert_eq!((code, out), (0, 4), "kind {kind}: {err}");
    }
}

#[test]
fn gigantic_unmatched_key_including_one_sharing_a_long_prefix_with_the_name() {
    for prefix in ["", "features", "fea\\u0074ures"] {
        let (code, out, err) = stream("--array-field features -", vec![(v(&format!("{{\"{prefix}")), 1), (vec![b'k'; MIB], 201), (v("\":1,\"features\":[7]}"), 1)]);
        assert_eq!((code, out), (0, 2), "prefix {prefix:?}: {err}");
    }
    // 200 MiB of lone-surrogate escapes in a key: grammatical, skipped, still O(1)
    let (code, out, err) = stream("--array-field features -", vec![(v("{\"\\ud800"), 1), (v("\\ud800"), 30 * MIB), (v("\":1,\"features\":[]}"), 1)]);
    assert_eq!((code, out), (0, 0), "{err}");
    // a gigantic key that equals nothing but whose tail would match: still no match
    let (code, _, _) = stream("--array-field features -", vec![(v("{\"x"), 1), (vec![b'k'; MIB], 201), (v("features\":[1]}"), 1)]);
    assert_eq!(code, 2);
}

// ---- 9: empty and minimal ----

#[test]
fn empty_and_minimal_outputs() {
    ok("features", r#"{"features":[]}"#, "");
    ok("features", r#"{"x":1,"features":[ ],"y":[1,2]}"#, "");
    ok("features", " \r\n{ \"features\" : [ 1 ] } \r\n", "1\n");
    ok("features", "{\"x\":{\"features\":[9]},\"features\":[1]}", "1\n");
    ok("features", "{\"x\":[\"features\"],\"features\":[1],\"y\":\"features\"}", "1\n");
    ok("features", r#"{"features":[[1,[2]],{"a":[{}]}]}"#, "[1,[2]]\n{\"a\":[{}]}\n");
    let d = scratch("field_empty");
    let p = d.join("e.out");
    let o = run(&["--array-field", "f", "--output", p.to_str().unwrap(), "-"], br#"{"x":1,"f":[],"y":null}"#);
    assert_eq!(o.code, 0);
    assert_eq!(fs::read(&p).unwrap(), b"");
}

#[test]
fn skipped_strings_hide_structure() {
    ok("f", r#"{"a":"}]\"}","b":["\\","x"],"f":[1],"c":{"k\"}":"\\"}}"#, "1\n");
    ok("f", r#"{"a\"f":[0],"f":["f\"]"]}"#, "\"f\\\"]\"\n");
}

// ---- 10: usage and compatibility ----

#[test]
fn usage_errors_exit_64() {
    for args in [&["--array-field", "a", "--array-field", "b", "-"][..], &["--array-field", "a", "--array-field=b", "-"], &["--array-field"], &["--array-field", "a"]] {
        let o = run(args, b"{}");
        assert_eq!(o.code, 64, "{args:?}");
        assert!(o.stderr.contains("Usage"), "{args:?}: {}", o.stderr);
    }
    // a lone `--array-field=` is the empty name (legal), not a usage error
    assert_eq!(run(&["--array-field=", "-"], b"{}").code, 2);
}

#[cfg(unix)]
#[test]
fn non_utf8_name_is_a_usage_error() {
    use std::os::unix::ffi::OsStrExt;
    let o = Command::new(BIN).arg("--array-field").arg(std::ffi::OsStr::from_bytes(b"f\xff")).arg("-").stdin(Stdio::null()).output().unwrap();
    assert_eq!(o.status.code(), Some(64));
    let o = Command::new(BIN).arg(std::ffi::OsStr::from_bytes(b"--array-field=f\xff")).arg("-").stdin(Stdio::null()).output().unwrap();
    assert_eq!(o.status.code(), Some(64));
}

#[test]
fn name_may_start_with_dash_and_options_combine_in_any_order() {
    ok("-x", r#"{"-x":[1]}"#, "1\n");
    let o = run(&["--max-depth", "9", "--array-field=f", "--", "-"], br#"{"f":[1]}"#);
    assert_eq!((o.code, o.stdout.as_slice()), (0, &b"1\n"[..]));
}

#[test]
fn help_documents_the_flag() {
    let o = run(&["--help"], b"");
    assert_eq!(o.code, 0);
    assert!(String::from_utf8_lossy(&o.stdout).contains("--array-field NAME"));
}

#[test]
fn file_input_matches_stdin_and_missing_file_is_66() {
    let d = scratch("field_file");
    let p = d.join("in.json");
    fs::write(&p, br#"{"a":1,"f":[1,2]}"#).unwrap();
    let o = run(&["--array-field", "f", p.to_str().unwrap()], b"");
    assert_eq!((o.code, o.stdout.as_slice()), (0, &b"1\n2\n"[..]));
    assert_eq!(run(&["--array-field", "f", d.join("nope").to_str().unwrap()], b"").code, 66);
}

#[test]
fn consumer_closing_early_is_74_in_field_mode() {
    let d = scratch("field_epipe");
    let p = d.join("big.json");
    let mut f = std::io::BufWriter::new(fs::File::create(&p).unwrap());
    f.write_all(b"{\"f\":[").unwrap();
    for _ in 0..400_000 { f.write_all(b"\"0123456789\",").unwrap(); }
    f.write_all(b"1]}").unwrap();
    drop(f);
    let mut c = Command::new(BIN).args(["--array-field", "f", p.to_str().unwrap()]).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut so = c.stdout.take().unwrap();
    let mut one = [0u8; 1];
    so.read_exact(&mut one).unwrap();
    drop(so);
    assert_eq!(c.wait().unwrap().code(), Some(74));
}

// ---- differential against Python's json (tests/oracle_field.py) ----

struct R(u64);
impl R { fn n(&mut self, m: usize) -> usize { self.0 ^= self.0 << 13; self.0 ^= self.0 >> 7; self.0 ^= self.0 << 17; (self.0 % m as u64) as usize } }

#[test]
fn field_mutations_agree_with_python_json_oracle() {
    // (name, seed document)
    let seeds: Vec<(&str, &[u8])> = vec![
        ("features", br#"{"type":"FC","features":[{"a":[1,2.5e10,-0,"x\n\u00e9"]},2,"s",null,true],"meta":{"n":1,"m":[{}]}}"#),
        ("features", br#"{"meta":{"features":[9]},"features":[12345678901234567890,1e400,"\\","\"",[],{}],"z":"]}"}"#),
        ("features", br#"{"\u0066eatures":[1,{"k":"v","k":"w"}],"a":[1,[2]],"b":"\ud800"}"#),
        ("😀", "{\"x\":1,\"\\ud83d\\ude00\":[1,{\"b\":null}], \"\\ud800\\ud800\":2 ,\"y\":[]}".as_bytes()),
        ("", br#"{"":[true,false,null],"a":{"b":{"c":[1]}}}"#),
        ("é", "{ \"a\" : \"é😀\" , \"\\u00e9\" : [ 1E-5 , {\"k\" : [ ] } ] }\n".as_bytes()),
    ];
    let toks: &[&[u8]] = &[b"[", b"]", b"{", b"}", b",", b":", b"\"", b"\\", b"-", b".", b"e", b"0", b"1", b" ", b"\n", b"\t", b"\x00", b"\xff", b"\xc3", b"\xa9", b"\xe2\x82", b"\xed\xa0\x80", b"NaN", b"true", b"nul", b"\\u00", b"\\ud800", b"\\udc00", b"\\ud83d", b"\\ude00", b"\\u0066", b"features", b"\"features\":[]", b"\"f\\u0065atures\":[2]", b",\"features\":[1]", b"\xef\xbb\xbf", b"\\u", b"{\"features\":", b"\"\\ud83d\\ude00\":3"];
    let d = scratch("field_diff");
    let mut r = R(0xD1B54A32D192ED03);
    let total = 3000;
    let mut names = Vec::new();
    for i in 0..total {
        let (name, seed) = seeds[r.n(seeds.len())];
        let mut v = seed.to_vec();
        for _ in 0..r.n(4) {
            let p = r.n(v.len() + 1);
            match r.n(3) {
                0 => { let t = toks[r.n(toks.len())]; v.splice(p..p, t.iter().copied()); }
                1 if p < v.len() => { v.remove(p); }
                _ if p < v.len() => { let t = toks[r.n(toks.len())]; v.splice(p..p + 1, t.iter().copied()); }
                _ => {}
            }
        }
        fs::write(d.join(i.to_string()), &v).unwrap();
        names.push(name);
    }
    for (i, name) in names.iter().enumerate() {
        let o = Command::new(BIN).args(["--array-field", name, d.join(i.to_string()).to_str().unwrap()]).stdin(Stdio::null()).stderr(Stdio::null()).output().unwrap();
        let code = o.status.code().unwrap_or(-1);
        assert!(code == 0 || code == 2, "case {i}: unexpected code {code}");
        fs::write(d.join(format!("{i}.out")), &o.stdout).unwrap();
        fs::write(d.join(format!("{i}.code")), code.to_string()).unwrap();
        fs::write(d.join(format!("{i}.name")), name).unwrap();
    }
    let o = Command::new("python3").args(["-I", "tests/oracle_field.py", d.to_str().unwrap(), &total.to_string()]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let report = String::from_utf8(o.stdout).unwrap();
    let (mut valid, mut invalid, mut mismatches) = (0, 0, Vec::new());
    for l in report.lines() {
        match l.split_once(' ') {
            Some(("VALID", _)) => valid += 1,
            Some(("INVALID", _)) => invalid += 1,
            _ => mismatches.push(l.to_string()),
        }
    }
    assert!(mismatches.is_empty(), "{} mismatches, first: {:?}", mismatches.len(), &mismatches[..mismatches.len().min(3)]);
    assert_eq!(valid + invalid, total);
    assert!(valid > 200 && invalid > 200, "corpus not balanced: {valid}/{invalid}");
}

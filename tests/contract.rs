//! M1 public contract, tested through the real binary. Expectations are hand-written literals;
//! the only independent oracle is Python's json module (tests/oracle.py), never serde.
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
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

fn stdin(input: &[u8]) -> Out { run(&["-"], input) }

#[test]
fn invalid_leaf_syntax_is_rejected_with_exit_2() {
    for s in [
        r#"[{"a":}]"#, "[01]", "[tru]", "[1.]", "[1e]", "[-]", "[[1 2]]", "[{a:1}]", r#"["\q"]"#, r#"["\u12G4"]"#,
        "[\"a\tb\"]", "[NaN]", "[Infinity]", "[-Infinity]", "[+1]", "[.5]", "['a']", r#"[{"a":1,}]"#, r#"[{"a" 1}]"#, "[[1,]]",
        "[nul]", "[1e+]", "[0x10]", "[--1]", "[\"a\\u00\"]",
    ] {
        let o = stdin(s.as_bytes());
        assert_eq!(o.code, 2, "{s:?} stderr={}", o.stderr);
        assert!(!o.stderr.is_empty(), "{s:?}: error message expected");
    }
}

#[test]
fn invalid_utf8_is_rejected_anywhere_including_truncated_tail() {
    let cases: &[&[u8]] = &[
        b"[\"\xff\"]", b"[\"\xc3\x28\"]", b"[\"\xe2\x82\"]", b"[\"\xc0\x80\"]", b"[\"\xed\xa0\x80\"]", b"[\"\xf4\x90\x80\x80\"]",
        b"\xef\xbb\xbf[1]", b"[1]\xff", b"[1,\xc3]", b"[\"\xe2\x82",
    ];
    for c in cases {
        let o = stdin(c);
        assert_eq!(o.code, 2, "{c:?} stderr={}", o.stderr);
    }
    assert_eq!(Command::new(BIN).arg("fixtures-extra/invalid_utf8.json").stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap().code(), Some(2));
}

#[test]
fn valid_lexemes_survive_byte_for_byte() {
    let big = "9".repeat(1000);
    let doc = format!(
        "[1e400, -12345678901234567890123,0.1E-99999999999999999999 ,{big},{{\"a\":1,\"a\":2}},\"\\ud800\",\"é😀\", -0.0 ,[1e2,{{}}]]\n\n  "
    );
    let want = format!(
        "1e400\n-12345678901234567890123\n0.1E-99999999999999999999\n{big}\n{{\"a\":1,\"a\":2}}\n\"\\ud800\"\n\"é😀\"\n-0.0\n[1e2,{{}}]\n"
    );
    let o = stdin(doc.as_bytes());
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(String::from_utf8(o.stdout).unwrap(), want);
}

#[test]
fn trailing_data_after_root_is_rejected() {
    for s in ["[1] [2]", "[1]x", "[1]\0", "[1]]", "[1]{}", "[] 1", "[1]\n[2]\n"] {
        assert_eq!(stdin(s.as_bytes()).code, 2, "{s:?}");
    }
    assert_eq!(stdin(b"[1]\r\n \t\n").code, 0);
    assert_eq!(stdin(b"[]").code, 0);
}

#[test]
fn non_array_or_truncated_roots_are_rejected() {
    for s in ["", " ", "{}", "1", "\"a\"", "null", "[", "[1", "[1,", "[\"abc"] {
        assert_eq!(stdin(s.as_bytes()).code, 2, "{s:?}");
    }
}

fn nested(total_depth: usize) -> Vec<u8> {
    // root array counts as depth 1
    let inner = total_depth - 1;
    format!("[{}{}]", "[".repeat(inner), "]".repeat(inner)).into_bytes()
}

#[test]
fn depth_limit_includes_root_and_applies_to_whole_file_at_once() {
    let d = scratch("depth");
    for (depth, extra, want) in [
        (256, &[][..], 0), (257, &[][..], 2), (1, &["--max-depth", "1"][..], 0), (2, &["--max-depth", "1"][..], 2),
        (3, &["--max-depth", "3"][..], 0), (4, &["--max-depth=3"][..], 2),
    ] {
        let f = d.join(format!("d{depth}.json"));
        fs::write(&f, nested(depth)).unwrap();
        let mut args: Vec<&str> = extra.to_vec();
        args.push(f.to_str().unwrap());
        let o = run(&args, b"");
        assert_eq!(o.code, want, "depth {depth} {extra:?}: {}", o.stderr);
        if want == 2 { assert!(o.stderr.contains("depth"), "{}", o.stderr); }
    }
    // objects count as nesting too: root + {} = 2
    assert_eq!(run(&["--max-depth", "1", "-"], b"[{}]").code, 2);
    assert_eq!(run(&["--max-depth", "2", "-"], b"[{\"a\":[]}]").code, 2);
    assert_eq!(run(&["--max-depth", "3", "-"], b"[{\"a\":[]}]").code, 0);
}

fn vm_limited(limit_kib: u64, args: &str) -> Command {
    let mut c = Command::new("sh");
    c.arg("-c").arg(format!("ulimit -v {limit_kib}; exec \"$0\" {args}")).arg(BIN);
    c
}

#[test]
fn absurdly_deep_input_fails_cleanly_with_low_memory() {
    let mut c = vm_limited(300_000, "-").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
    let mut sin = c.stdin.take().unwrap();
    let t = std::thread::spawn(move || { let blk = vec![b'['; 1 << 20]; for _ in 0..16 { if sin.write_all(&blk).is_err() { break; } } });
    let o = c.wait_with_output().unwrap();
    let _ = t.join();
    assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stderr).contains("depth"));
}

#[test]
fn deep_but_allowed_nesting_validates_without_recursion() {
    let n = 200_000usize;
    let d = scratch("deep_ok");
    let f = d.join("deep.json");
    fs::write(&f, nested(n)).unwrap();
    let st = vm_limited(300_000, &format!("--max-depth {} {}", n, f.display())).stdout(Stdio::null()).stderr(Stdio::inherit()).status().unwrap();
    assert_eq!(st.code(), Some(0));
}

#[test]
fn record_over_200mib_streams_without_whole_record_allocation() {
    let mut c = vm_limited(150_000, "-").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut sin = c.stdin.take().unwrap();
    let mut sout = c.stdout.take().unwrap();
    let t = std::thread::spawn(move || {
        let blk = vec![b'a'; 1 << 20];
        sin.write_all(b"[\"").unwrap();
        for _ in 0..201 { sin.write_all(&blk).unwrap(); }
        sin.write_all(b"\",1]").unwrap();
    });
    let mut n = 0u64;
    let mut buf = vec![0u8; 1 << 20];
    loop { let k = sout.read(&mut buf).unwrap(); if k == 0 { break; } n += k as u64; }
    t.join().unwrap();
    let o = c.wait_with_output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(n, 201 * (1 << 20) + 2 + 1 + 2);
}

#[test]
fn usage_errors_exit_64() {
    for args in [
        &[][..], &["--max-depth", "0", "-"], &["--max-depth", "-1", "-"], &["--max-depth", "abc", "-"], &["--max-depth", "", "-"],
        &["--max-depth", "99999999999999999999", "-"], &["--max-depth", "+5", "-"], &["--max-depth"], &["--max-depth=0", "-"],
        &["--bogus", "-"], &["a.json", "b.json"], &["--max-depth", "5", "--max-depth", "6", "-"], &["--output"],
        &["--output", "x", "--output", "y", "-"], &["--"], &["-x"],
    ] {
        let o = run(args, b"");
        assert_eq!(o.code, 64, "{args:?}");
        assert!(o.stderr.contains("Usage") || o.stderr.contains("usage"), "{args:?}: {}", o.stderr);
    }
}

#[test]
fn help_and_version_exit_0_on_stdout() {
    for a in ["--help", "-h"] {
        let o = run(&[a], b"");
        assert_eq!(o.code, 0);
        let s = String::from_utf8(o.stdout).unwrap();
        for w in ["Usage", "--max-depth", "--output", "64", "66", "73", "74"] { assert!(s.contains(w), "{a}: missing {w}"); }
    }
    let o = run(&["--version"], b"");
    assert_eq!(o.code, 0);
    assert_eq!(String::from_utf8(o.stdout).unwrap(), format!("json-stream {}\n", env!("CARGO_PKG_VERSION")));
}

#[test]
fn double_dash_allows_dash_prefixed_filename_and_missing_file_is_66() {
    let d = scratch("dd");
    fs::write(d.join("-odd.json"), "[1]").unwrap();
    let o = Command::new(BIN).current_dir(&d).args(["--", "-odd.json"]).output().unwrap();
    assert_eq!((o.status.code(), o.stdout), (Some(0), b"1\n".to_vec()));
    assert_eq!(run(&["--", "nope.json"], b"").code, 66);
    assert_eq!(run(&["--max-depth", "4", "nope.json"], b"").code, 66);
}

fn leftovers(d: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into()).collect();
    v.sort();
    v
}

#[test]
fn output_file_is_published_only_after_full_validation() {
    let d = scratch("out_ok");
    let dest = d.join("o.ndjson");
    let o = run(&["--output", dest.to_str().unwrap(), "-"], b"[1,{\"a\":[2]}]");
    assert_eq!((o.code, o.stdout.len()), (0, 0), "{}", o.stderr);
    assert_eq!(fs::read(&dest).unwrap(), b"1\n{\"a\":[2]}\n");
    assert_eq!(leftovers(&d), ["o.ndjson"]);
}

#[test]
fn invalid_input_never_publishes_and_leaves_no_temp_files() {
    let d = scratch("out_bad");
    let dest = d.join("o.ndjson");
    // the first records are valid and large enough to have been written to the temp file before the fault
    let mut doc = b"[".to_vec();
    for _ in 0..50_000 { doc.extend(b"{\"a\":1},"); }
    doc.extend(b"[1 2]]");
    let o = run(&["--output", dest.to_str().unwrap(), "-"], &doc);
    assert_eq!(o.code, 2);
    assert_eq!(leftovers(&d), Vec::<String>::new());
}

#[test]
fn existing_destination_is_never_clobbered() {
    let d = scratch("out_exist");
    let dest = d.join("o.ndjson");
    fs::write(&dest, "precious").unwrap();
    let o = run(&["--output", dest.to_str().unwrap(), "-"], b"[1]");
    assert_eq!(o.code, 73, "{}", o.stderr);
    assert_eq!(fs::read(&dest).unwrap(), b"precious");
    assert_eq!(leftovers(&d), ["o.ndjson"]);
    // dangling symlink counts as existing too
    let l = d.join("link");
    std::os::unix::fs::symlink(d.join("elsewhere"), &l).unwrap();
    assert_eq!(run(&["--output", l.to_str().unwrap(), "-"], b"[1]").code, 73);
    assert!(!d.join("elsewhere").exists());
    assert_eq!(run(&["--output", d.join("no/such/dir/o").to_str().unwrap(), "-"], b"[1]").code, 73);
}

#[test]
fn concurrent_writers_to_one_destination_yield_exactly_one_complete_file() {
    let d = scratch("out_race");
    let dest = d.join("o.ndjson");
    let mut doc = b"[".to_vec();
    for i in 0..20_000 { doc.extend(format!("{i},").as_bytes()); }
    doc.extend(b"0]");
    let hs: Vec<_> = (0..6).map(|_| { let (dest, doc) = (dest.clone(), doc.clone()); std::thread::spawn(move || run(&["--output", dest.to_str().unwrap(), "-"], &doc).code) }).collect();
    let codes: Vec<i32> = hs.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(codes.iter().filter(|&&c| c == 0).count(), 1, "{codes:?}");
    assert!(codes.iter().all(|&c| c == 0 || c == 73), "{codes:?}");
    assert_eq!(fs::read(&dest).unwrap().iter().filter(|&&b| b == b'\n').count(), 20_001);
    assert_eq!(leftovers(&d), ["o.ndjson"]);
}

#[test]
fn stdout_stays_streaming_and_partial_on_failure() {
    let o = stdin(b"[{\"a\":1},{\"a\":2},[1 2]]");
    assert_eq!(o.code, 2);
    assert!(o.stdout.starts_with(b"{\"a\":1}\n{\"a\":2}\n"), "{:?}", String::from_utf8_lossy(&o.stdout));
}

#[test]
fn stdout_write_failure_exits_74() {
    let full = fs::OpenOptions::new().write(true).open("/dev/full").unwrap();
    let st = Command::new(BIN).arg("-").stdin(Stdio::piped()).stdout(full).stderr(Stdio::null()).spawn().and_then(|mut c| { c.stdin.take().unwrap().write_all(b"[1,2]").unwrap(); c.wait() }).unwrap();
    assert_eq!(st.code(), Some(74));
}

#[test]
fn consumer_closing_early_is_a_failure_not_success() {
    let mut c = Command::new(BIN).arg("-").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut sin = c.stdin.take().unwrap();
    let mut sout = c.stdout.take().unwrap();
    let t = std::thread::spawn(move || { let _ = sin.write_all(b"["); for _ in 0..2_000_000 { if sin.write_all(b"1,").is_err() { return; } } let _ = sin.write_all(b"1]"); });
    let mut b = [0u8; 4];
    sout.read_exact(&mut b).unwrap();
    drop(sout);
    let st = c.wait().unwrap();
    let _ = t.join();
    assert_eq!(st.code(), Some(74));
}

#[test]
fn directory_as_input_is_a_read_error_74() {
    assert_eq!(run(&["."], b"").code, 74);
}

// ---- differential: random mutations judged by Python's json (strict UTF-8, lexeme numbers) ----

struct R(u64);
impl R { fn n(&mut self, m: usize) -> usize { self.0 ^= self.0 << 13; self.0 ^= self.0 >> 7; self.0 ^= self.0 << 17; (self.0 % m as u64) as usize } }

#[test]
fn random_mutations_agree_with_python_json_oracle() {
    let seeds: Vec<&[u8]> = vec![
        b"[1,2.5e10,-0,\"a\\n\\u00e9\\ud800\",null,true,false]", b"[{\"a\":[1,{\"b\":null}],\"c\":\"x\"},[],{}]",
        "[ \"é😀\" , 1E-5 , {\"k\" : [ ] } ]\n".as_bytes(), b"[12345678901234567890,1e400,\"\\\\\",\"\\\"\"]",
    ];
    let toks: &[&[u8]] = &[b"[", b"]", b"{", b"}", b",", b":", b"\"", b"\\", b"-", b".", b"e", b"0", b"1", b" ", b"\n", b"\t", b"\x00", b"\xff", b"\xc3", b"\xa9", b"\xe2\x82", b"\xed\xa0\x80", b"NaN", b"true", b"nul", b"\\u00", b"\\ud800", b"\xef\xbb\xbf"];
    let d = scratch("diff");
    let mut r = R(0x9E3779B97F4A7C15);
    let total = 2500;
    for i in 0..total {
        let mut v = seeds[r.n(seeds.len())].to_vec();
        for _ in 0..1 + r.n(3) {
            let p = r.n(v.len() + 1);
            match r.n(3) {
                0 => { let t = toks[r.n(toks.len())]; v.splice(p..p, t.iter().copied()); }
                1 if p < v.len() => { v.remove(p); }
                _ if p < v.len() => { let t = toks[r.n(toks.len())]; v.splice(p..p + 1, t.iter().copied()); }
                _ => {}
            }
        }
        fs::write(d.join(i.to_string()), v).unwrap();
    }
    let o = Command::new("python3").args(["-I", "tests/oracle.py", d.to_str().unwrap(), &total.to_string()]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let verdicts: Vec<(usize, bool)> = String::from_utf8(o.stdout).unwrap().lines().map(|l| { let (a, b) = l.split_once(' ').unwrap(); (a.parse().unwrap(), b == "1") }).collect();
    assert_eq!(verdicts.len(), total);
    let (mut valid, mut bad) = (0, 0);
    for (i, ok) in verdicts {
        let f = d.join(i.to_string());
        let code = Command::new(BIN).arg(&f).stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap().code();
        assert_eq!(code == Some(0), ok, "case {i}: {:?} cli={code:?} oracle_valid={ok}", String::from_utf8_lossy(&fs::read(&f).unwrap()));
        assert!(code == Some(0) || code == Some(2), "case {i}: unexpected code {code:?}");
        if ok { valid += 1 } else { bad += 1 }
    }
    assert!(valid > 100 && bad > 100, "corpus not balanced: {valid}/{bad}");
}

// ---- process-boundary regressions (Unix only: raw-bytes argv, closed fd 1).
// A closed fd 1 cannot be detected: Rust's runtime reopens it as /dev/null before main() (verified:
// readlink /proc/self/fd/1 == /dev/null), identical to `>/dev/null`. Only --output is reliable then. ----
#[cfg(unix)]
mod io_boundaries {
    use super::*;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::ExitStatusExt;

    fn status(c: &mut Command) -> (Option<i32>, Option<i32>, String) {
        let o = c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).output().unwrap();
        (o.status.code(), o.status.signal(), String::from_utf8_lossy(&o.stderr).into())
    }
    /// Runs BIN with fd 1 closed before exec.
    fn closed_stdout(args: &[&str]) -> (Option<i32>, Option<i32>, String) {
        let mut c = Command::new("sh");
        c.args(["-c", "exec \"$0\" \"$@\" >&-", BIN]).args(args);
        status(&mut c)
    }

    #[test]
    fn non_utf8_input_filename_is_a_normal_open_error_not_an_abort() {
        let (code, sig, err) = status(Command::new(BIN).arg(std::ffi::OsStr::from_bytes(b"/nonexistent/\xff.json")));
        assert_eq!((code, sig), (Some(66), None), "{err}");
    }

    #[test]
    fn non_utf8_input_and_output_filenames_work() {
        let d = scratch("nonutf8");
        let inp = d.join(std::ffi::OsStr::from_bytes(b"in-\xff.json"));
        let out = d.join(std::ffi::OsStr::from_bytes(b"out-\xfe.ndjson"));
        fs::write(&inp, "[1,{\"a\":2}]").unwrap();
        let (code, sig, err) = status(Command::new(BIN).arg("--output").arg(&out).arg(&inp));
        assert_eq!((code, sig), (Some(0), None), "{err}");
        assert_eq!(fs::read(&out).unwrap(), b"1\n{\"a\":2}\n");
    }

    #[test]
    fn readonly_stdout_fd_is_a_write_error_not_silent_success() {
        let d = scratch("ro-stdout");
        let (f, o) = (d.join("in.json"), d.join("existing.txt"));
        fs::write(&f, "[1,2]").unwrap();
        fs::write(&o, "keep").unwrap();
        let out = Command::new(BIN).arg(&f).stdin(Stdio::null()).stdout(fs::File::open(&o).unwrap()).stderr(Stdio::piped()).output().unwrap();
        assert_eq!(out.status.code(), Some(74), "{}", String::from_utf8_lossy(&out.stderr));
        assert!(String::from_utf8_lossy(&out.stderr).contains("write"));
        assert_eq!(fs::read(&o).unwrap(), b"keep");
    }

    #[test]
    fn closed_stdout_with_output_file_succeeds() {
        let d = scratch("closed1-out");
        let (f, o) = (d.join("in.json"), d.join("o.ndjson"));
        fs::write(&f, "[1,2,3]").unwrap();
        let (code, sig, err) = closed_stdout(&["--output", o.to_str().unwrap(), f.to_str().unwrap()]);
        assert_eq!((code, sig), (Some(0), None), "{err}");
        assert_eq!(fs::read(&o).unwrap(), b"1\n2\n3\n");
    }
}

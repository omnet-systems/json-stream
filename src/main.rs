//! json-stream: validate ONE UTF-8 JSON document whose root is an array and emit each top-level
//! element as one raw line (see README / --help for the contract and exit codes).
mod field;
mod scan;
mod validate;

use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::Path;
use std::process::exit;
use validate::{validate_with, Fail};

const USAGE: &str = "Usage: json-stream [--max-depth N] [--output PATH] [--array-field NAME] [--] FILE|-";

const HELP: &str = "json-stream: validate one UTF-8 JSON document whose root is an array and print each
top-level element as one line (token-preserving: number/string lexemes, duplicate keys and
lone-surrogate escapes are kept as written).

Usage: json-stream [--max-depth N] [--output PATH] [--array-field NAME] [--] FILE|-

  FILE|-          input file, or - for stdin
  --array-field NAME
                  the root is an object: print the elements of its array member NAME instead
                  (NAME is matched after JSON unescaping, must occur exactly once, must be an array;
                  all other members are validated and skipped; the root object counts as depth 1)
  --max-depth N   nesting limit, root array/object counts as 1 (positive integer, default 256)
  --output PATH   write lines to PATH, published atomically ONLY if the whole input is valid;
                  never overwrites: an existing PATH is left untouched (exit 73)
  -h, --help      this text      --version   print version

Without --output, lines stream to stdout and may be partial when the input turns out invalid:
trust stdout only if the exit status is 0 (use `set -o pipefail` in pipelines).

Exit status:
  0   input valid and all output written
  2   invalid data: JSON grammar, non-UTF-8, root not an array (--array-field: not an object, NAME
      missing / duplicated / not an array), trailing data, depth exceeded
  64  usage error
  66  input file cannot be opened
  73  --output cannot be created or already exists
  74  I/O error reading input or writing output (including a consumer closing stdout early)
";

fn usage_err(msg: &str) -> ! {
    eprintln!("json-stream: {msg}\n{USAGE}");
    exit(64);
}

/// `--output=<raw bytes>` whose value is not UTF-8 (only reachable on Unix).
fn inline_output(a: &OsStr) -> Option<OsString> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        a.as_bytes().strip_prefix(b"--output=").map(|v| OsString::from_vec(v.to_vec()))
    }
    #[cfg(not(unix))]
    { let _ = a; None }
}

fn main() {
    let mut args = std::env::args_os().skip(1);
    let (mut depth, mut output, mut file): (Option<usize>, Option<OsString>, Option<OsString>) = (None, None, None);
    let mut array_field: Option<String> = None;
    let mut only_positional = false;
    while let Some(a) = args.next() {
        // a non-UTF-8 argument cannot be a known option name: it is a path (or an inline --output value)
        let Some(t) = a.to_str() else {
            if !only_positional && a.to_string_lossy().starts_with("--array-field=") { usage_err("--array-field NAME must be valid UTF-8"); }
            if !only_positional { if let Some(v) = inline_output(&a) {
                if output.replace(v).is_some() { usage_err("--output given twice"); }
                continue;
            } }
            if file.replace(a).is_some() { usage_err("more than one input"); }
            continue;
        };
        if only_positional || t == "-" || !t.starts_with('-') {
            if file.replace(a).is_some() { usage_err("more than one input"); }
            continue;
        }
        let (name, inline) = match t.split_once('=') { Some((n, v)) if n.starts_with("--") => (n, Some(OsString::from(v))), _ => (t, None) };
        match name {
            "--" => only_positional = true,
            "-h" | "--help" => { print!("{HELP}"); exit(0) },
            "--version" => { println!("json-stream {}", env!("CARGO_PKG_VERSION")); exit(0) },
            "--array-field" => {
                let Some(v) = inline.or_else(|| args.next()) else { usage_err("--array-field needs a value") };
                let Ok(v) = v.into_string() else { usage_err("--array-field NAME must be valid UTF-8") };
                if array_field.replace(v).is_some() { usage_err("--array-field given twice"); }
            }
            "--max-depth" | "--output" => {
                let Some(v) = inline.or_else(|| args.next()) else { usage_err(&format!("{name} needs a value")) };
                if name == "--output" {
                    if output.replace(v).is_some() { usage_err("--output given twice"); }
                } else {
                    if depth.is_some() { usage_err("--max-depth given twice"); }
                    let v = v.to_str().unwrap_or("");
                    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) { usage_err("--max-depth must be a positive integer"); }
                    depth = match v.parse::<usize>() { Ok(n) if n > 0 => Some(n), _ => usage_err("--max-depth must be a positive integer") };
                }
            }
            _ => usage_err(&format!("unknown option {t}")),
        }
    }
    let Some(file) = file else { usage_err("missing FILE|-") };
    let depth = depth.unwrap_or(256);

    let input: Box<dyn std::io::Read> = if file == "-" {
        Box::new(io::stdin().lock())
    } else {
        match std::fs::File::open(&file) {
            Ok(f) => Box::new(f),
            Err(e) => { eprintln!("json-stream: {}: {e}", Path::new(&file).display()); exit(66); }
        }
    };

    let code = match output {
        None => finish(stdout_writer().map_err(|e| Fail::Io(format!("write: {e}"))).and_then(|w| validate_with(input, w, depth, array_field.as_deref())).and_then(|mut w| w.flush().map_err(|e| Fail::Io(format!("write: {e}"))))),
        Some(path) => to_file(input, Path::new(&path), depth, array_field.as_deref()),
    };
    exit(code);
}

/// std's Stdout swallows EBADF (so a closed fd 1 looks like success); a dup'd File reports it like any write error.
#[cfg(unix)]
fn stdout_writer() -> io::Result<Box<dyn Write>> {
    use std::os::fd::AsFd;
    Ok(Box::new(io::BufWriter::with_capacity(256 * 1024, std::fs::File::from(io::stdout().as_fd().try_clone_to_owned()?))))
}
#[cfg(not(unix))]
fn stdout_writer() -> io::Result<Box<dyn Write>> { Ok(Box::new(io::stdout().lock())) }

fn finish(r: Result<(), Fail>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(Fail::Data(m)) => { eprintln!("json-stream: invalid input: {m}"); 2 }
        Err(Fail::Io(m)) => { eprintln!("json-stream: {m}"); 74 }
    }
}

/// Temp file in the destination directory, hard-linked into place only on success (no-clobber, race-safe).
fn to_file(input: Box<dyn std::io::Read>, path: &Path, depth: usize, field: Option<&str>) -> i32 {
    let cant = |m: String| -> i32 { eprintln!("json-stream: --output {}: {m}", path.display()); 73 };
    if std::fs::symlink_metadata(path).is_ok() { return cant("already exists".into()); }
    let dir = match path.parent() { Some(p) if !p.as_os_str().is_empty() => p, _ => Path::new(".") };
    let tmp = match tempfile::Builder::new().prefix(".json-stream-").suffix(".tmp").tempfile_in(dir) {
        Ok(t) => t,
        Err(e) => return cant(e.to_string()),
    };
    let w = io::BufWriter::with_capacity(256 * 1024, tmp);
    let w = match validate_with(input, w, depth, field) {
        Ok(w) => w,
        Err(f) => return finish(Err(f)), // temp file dropped = removed
    };
    let tmp = match w.into_inner() { Ok(t) => t, Err(e) => return finish(Err(Fail::Io(format!("write: {e}")))) };
    if let Err(e) = tmp.as_file().sync_all() { return finish(Err(Fail::Io(format!("write: {e}")))); }
    match tmp.persist_noclobber(path) {
        Ok(_) => 0,
        Err(e) if e.error.kind() == io::ErrorKind::AlreadyExists => cant("already exists".into()),
        Err(e) => cant(e.error.to_string()),
    }
}

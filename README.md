# json-stream

`json-stream` turns a JSON array into one JSON value per line, preserving number
and string lexemes and duplicate keys. For Linux GIS imports, `--array-field
features` extracts a GeoJSON FeatureCollection's features without loading the
whole document into memory. Version **0.2.0-alpha.2**; tested on Linux only.
The Linux x86_64 binary requires glibc symbols through `GLIBC_2.34`; compatibility
with other hosts and libc implementations has not been tested.

## Install or build

From the [GitHub releases](https://github.com/omnet-systems/json-stream/releases),
extract the Linux x86_64 archive and install the executable somewhere on your PATH:

```sh
tar -xzf json-stream-0.2.0-alpha.2-linux-x86_64.tar.gz
install -Dm755 json-stream-0.2.0-alpha.2-linux-x86_64/json-stream "$HOME/.local/bin/json-stream"
```

Or build from source with Rust and Cargo:

```sh
cargo build --release --locked
./target/release/json-stream --version
```

Release packaging requires Python 3.11 or newer. For a path-remapped offline
release build, populate `.work/cargo-home` with the
locked dependencies, then run `python3 release/build.py`. Package it with
`python3 release/package.py`; the archive is written to `dist/` and includes the
MIT license and dependency notices. Repackaging the same inputs produces the
same archive bytes; compiler/toolchain differences can change the binary.

## GIS import example

With `jq` installed, select road features and emit a scalar projection:

```sh
set -o pipefail
json-stream --array-field features roads.geojson \
  | jq -c 'select(.properties.type=="Major Highway") | .properties.sov_a3'
```

To prepare an import file only after complete input validation:

```sh
json-stream --array-field features --output roads.jsonl roads.geojson
```

The destination must not already exist. Use `-` as the input for stdin. Without
`--array-field`, the root must be an array. Run `json-stream --help` for all
options and exit codes.

## Scope and output guarantees

- `--array-field` matches one literal root member name after JSON unescaping.
  The root must be an object and that member must occur exactly once as an array.
  Nested paths and wildcards are unsupported.
- The whole document is checked for JSON grammar, UTF-8, trailing data and nesting
  depth (default 256, including the root). Skipped siblings are validated too.
  This does not validate GeoJSON geometry, coordinate systems or GIS schemas.
- Elements stream without materializing the full document or a whole element.
  Number magnitude, duplicate keys inside elements and lone-surrogate escapes
  are accepted and preserved. Formatting whitespace can change; this is not
  byte-for-byte document copying. Downstream tools such as jq may rewrite tokens.
- Stdout can contain partial output before a later error. Trust it only on exit
  zero; use Bash `set -o pipefail` for pipelines. This does not roll back a
  downstream import that has already consumed records.
- `--output` writes a temporary file in the destination directory, syncs its
  contents, and publishes it without overwriting an existing path after successful
  validation. Linux files are created with mode 0600. A killed process may leave
  a temporary file. The directory is not synced, so power-loss durability is not
  guaranteed; filesystem support for atomic publication is required.

## Validation and measured scope

The existing suite requires Python 3, jq and a POSIX shell:

```sh
CARGO_HOME="$PWD/.work/cargo-home" cargo test --locked --offline
mkdir -p .work/package
tar -xzf dist/json-stream-0.2.0-alpha.2-linux-x86_64.tar.gz -C .work/package
python3 benchmarks/gis_validation.py \
  --binary .work/package/json-stream-0.2.0-alpha.2-linux-x86_64/json-stream \
  --self-check --out-dir .work/gis-selfcheck
```

For the full GIS comparison, omit `--self-check`, set `--input-dir
.work/gis-inputs --out-dir .work/gis-results`, and use `--download-pinned` once to
fetch the hash-checked public-domain Natural Earth roads corpus. Later runs are
offline. The script also generates a synthetic fourfold repetition.

Historical measurements on one host used binary SHA-256
`f41402d96fdd4e29cdd8b27fdb7dfdce68063bb6cff8586a52c13debf81348ca`.
On the 201.9 MB synthetic roads input, three rotated warm-cache rounds gave
median wall times of 6.691 s for `json-stream | jq`, 60.291 s for the tested
streaming jq recipe and 6.339 s for plain jq. All 18 timed runs across the two
input sizes matched the scalar oracle. With a separate 128 MiB virtual-memory
cap per process, both streaming arms succeeded on both inputs and plain jq
failed. This is not a container-wide memory limit or a CPU-efficiency claim.
These figures describe that exact binary, not a newly rebuilt release binary.
Synthetic repetition and one host do not establish general GIS performance;
GDAL and ijson were not compared.

MIT, copyright 2026 Omnet Systems. Dependency attributions are preserved in
[THIRD_PARTY_NOTICES.txt](release/THIRD_PARTY_NOTICES.txt).

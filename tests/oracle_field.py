#!/usr/bin/env python3 -I
"""Independent oracle for the --array-field differential. Usage: python3 -I oracle_field.py DIR N
For each case i, DIR has: i (input), i.name (NAME), i.out (binary stdout), i.code (binary exit status).
Verdict = input is one strictly-UTF-8 JSON object with EXACTLY ONE member whose decoded key == NAME
(lone-surrogate keys never equal a valid-UTF-8 NAME) and that member is an array. Numbers stay lexemes.
Prints "VALID i" / "INVALID i" when the binary agrees (valid: output lines == elements, in order, with
lexeme-exact numbers and duplicate keys kept), otherwise "MISMATCH i why"."""
import json, sys

class N(str):  # number lexeme
    pass

class O(list):  # object as ordered (key, value) pairs, duplicates kept
    pass

def bad_const(s):
    raise ValueError(s)

def canon(v):
    if isinstance(v, N): return ["N", str(v)]
    if isinstance(v, O): return ["O", [[k, canon(x)] for k, x in v]]
    if isinstance(v, list): return ["A", [canon(x) for x in v]]
    if isinstance(v, str): return ["S", v]
    return ["L", v]

def load(text):
    return json.loads(text, parse_int=N, parse_float=N, parse_constant=bad_const, object_pairs_hook=O)

d, n = sys.argv[1], int(sys.argv[2])
for i in range(n):
    raw = open(f"{d}/{i}", "rb").read()
    name = open(f"{d}/{i}.name", "rb").read().decode("utf-8")
    out = open(f"{d}/{i}.out", "rb").read()
    code = int(open(f"{d}/{i}.code").read())
    want = None
    try:
        root = load(raw.decode("utf-8"))
        if isinstance(root, O):
            hits = [x for k, x in root if k == name]
            if len(hits) == 1 and isinstance(hits[0], list) and not isinstance(hits[0], O):
                want = hits[0]
    except (ValueError, RecursionError):
        pass
    if want is None:
        print(("INVALID %d" if code == 2 else "MISMATCH %d accepted-invalid") % i)
        continue
    if code != 0:
        print(f"MISMATCH {i} rejected-valid")
        continue
    try:
        lines = out.decode("utf-8").split("\n")
        if lines.pop() != "" and True: raise ValueError("no final newline")
        got = [canon(load(l)) for l in lines]
    except (ValueError, RecursionError) as e:
        print(f"MISMATCH {i} output-unparseable {e}")
        continue
    if got != [canon(x) for x in want]:
        print(f"MISMATCH {i} elements-differ")
    elif any("\n" in l or "\r" in l for l in lines):
        print(f"MISMATCH {i} line-break")
    else:
        print(f"VALID {i}")

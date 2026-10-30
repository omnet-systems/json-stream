#!/usr/bin/env python3 -I
"""Independent oracle for the differential test. Usage: python3 -I oracle.py DIR N
Prints "<i> 1" if DIR/<i> is one strictly-UTF-8 JSON document whose root is an array, else "<i> 0".
Numbers stay lexemes (parse_int/parse_float=str); NaN/Infinity rejected; lone surrogate escapes and
duplicate keys accepted (the documented M1 grammar)."""
import json, sys

def bad_const(s):
    raise ValueError(s)

d, n = sys.argv[1], int(sys.argv[2])
for i in range(n):
    raw = open(f"{d}/{i}", "rb").read()
    try:
        v = json.loads(raw.decode("utf-8"), parse_int=str, parse_float=str, parse_constant=bad_const)
        ok = isinstance(v, list)
    except (ValueError, RecursionError):
        ok = False
    print(i, int(ok))

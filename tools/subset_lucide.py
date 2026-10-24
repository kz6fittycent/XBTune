#!/usr/bin/env python3
"""Build crates/xbtune-app/assets/fonts/lucide.ttf: the Lucide icons that
crates/xbtune-app/src/icons.rs names (`// lucide: <name>`), nothing else.

Usage (repo root), with lucide-static unpacked from npm
(`npm pack lucide-static@<version>`, see docs/PROVENANCE.md):
    python3 -I tools/subset_lucide.py <unpacked>/package
Needs fontTools (`pip install fonttools`, in a virtualenv).
Checks each codepoint in icons.rs against Lucide's codepoints.json.
"""
import json
import re
import sys

from fontTools import subset

pkg = sys.argv[1]
codepoints = json.load(open(f"{pkg}/font/codepoints.json"))
src = open("crates/xbtune-app/src/icons.rs").read()
wanted, bad = [], []
for esc, name in re.findall(r'"\\u\{([0-9a-f]+)\}"; // lucide: ([a-z0-9-]+)', src):
    cp = int(esc, 16)
    if codepoints.get(name) != cp:
        bad.append(f"{name}: icons.rs has U+{cp:04X}, Lucide has {hex(codepoints[name]) if name in codepoints else 'no such icon'}")
    wanted.append(cp)
if bad:
    sys.exit("\n".join(bad))
opts = subset.Options()
opts.layout_features = []
opts.name_IDs = ["*"]
opts.notdef_outline = True
font = subset.load_font(f"{pkg}/font/lucide.ttf", opts)
s = subset.Subsetter(opts)
s.populate(unicodes=wanted)
s.subset(font)
subset.save_font(font, "crates/xbtune-app/assets/fonts/lucide.ttf", opts)
print(f"{len(wanted)} icons")

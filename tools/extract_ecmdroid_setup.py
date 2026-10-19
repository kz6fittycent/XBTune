#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Extract EcmDroid's ECM Setup screen into data/ecm_setup.toml.

Usage: python3 -I tools/extract_ecmdroid_setup.py ECMDROID_CHECKOUT COMMIT > data/ecm_setup.toml

Reads app/src/main/res/xml/ecm_setup.xml (the screen) and
app/src/main/res/values/strings.xml (its English titles) from an EcmDroid
checkout (https://github.com/ecmdroid/ecmdroid, Copyright (C) 2012 Michel
Marti, GPL-3.0-or-later). This is EcmDroid source, not ecmdroid.db, so the
output is not restricted data (docs/PROVENANCE.md).

Output: groups in screen order, each with its title path (screen,
sub-screen, category) and items. An item is a switch ("bits": a key like
KBaro_Config[7], or [6,7] for bits that move together) or a number
("value": an EEPROM variable name). Alternatives are kept in order
("KBaro_Config[6]:KABP_Config[7]" means whichever the ECM has, as
EcmDroid's SetupFragment.readPrefs does). A missing title stays empty:
EcmDroid then uses the bit's or variable's name from the definition.
"""
import json
import os
import sys
import xml.etree.ElementTree as ET

A = "{http://schemas.android.com/apk/res/android}"


def s(v):
    return json.dumps(v, ensure_ascii=False)


def main():
    root_dir, commit = sys.argv[1], sys.argv[2]
    res = os.path.join(root_dir, "app", "src", "main", "res")
    strings = {}
    for e in ET.parse(os.path.join(res, "values", "strings.xml")).getroot():
        if e.tag == "string":
            # Android escapes: \' and \" in resources.
            strings[e.get("name")] = "".join(e.itertext()).replace("\\'", "'").replace('\\"', '"')

    def title(e):
        t = e.get(A + "title")
        if not t:
            return ""
        if t.startswith("@string/"):
            return strings[t[len("@string/"):]]
        return t

    groups = []

    def walk(e, path):
        items = []
        for c in e:
            kind = c.tag
            if kind in ("PreferenceScreen", "PreferenceCategory"):
                walk(c, path + [title(c)])
            elif kind in ("CheckBoxPreference", "EditTextPreference"):
                keys = [k.strip() for k in c.get(A + "key").split(":")]
                items.append(("bits" if kind == "CheckBoxPreference" else "value", keys, title(c)))
            else:
                raise SystemExit(f"unexpected element {kind}")
        if items:
            groups.append(([p for p in path if p], items))

    walk(ET.parse(os.path.join(res, "xml", "ecm_setup.xml")).getroot(), [])

    print("# EcmDroid's ECM Setup screen, extracted by tools/extract_ecmdroid_setup.py.")
    print("# From EcmDroid source (GPL-3.0-or-later, Copyright (C) 2012 Michel Marti),")
    print("# NOT from ecmdroid.db. See docs/PROVENANCE.md.")
    print(f"source = {s('ecmdroid ' + commit + ' app/src/main/res/xml/ecm_setup.xml, values/strings.xml')}")
    for path, items in groups:
        print()
        print("[[group]]")
        print(f"path = [{', '.join(s(p) for p in path)}]")
        for kind, keys, t in items:
            print(f"[[group.item]]")
            print(f"kind = {s(kind)}")
            print(f"keys = [{', '.join(s(k) for k in keys)}]")
            if t:
                print(f"title = {s(t)}")


if __name__ == "__main__":
    main()

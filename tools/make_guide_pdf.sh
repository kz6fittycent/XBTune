#!/usr/bin/env bash
# Build XBTune-Guide.pdf from the current wiki (CI: the Windows zips).
#   tools/make_guide_pdf.sh OUT.pdf LABEL      # from the repo root
# Clones the wiki, installs the pinned PDF tools in a throwaway virtualenv
# and runs tools/wiki_pdf.py. Needs git, python3 (3.10+) and Pango (on
# Ubuntu: libpango-1.0-0 libpangoft2-1.0-0; DejaVu fonts for code).
set -euo pipefail
out="$(realpath -m "$1")"
label="$2"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
git clone --quiet --depth 1 https://github.com/kz6fittycent/XBTune.wiki.git "$work/wiki"
python3 -m venv "$work/venv"
"$work/venv/bin/pip" install --quiet markdown==3.9 weasyprint==70.0
"$work/venv/bin/python" -I tools/wiki_pdf.py "$work/wiki" "$out" --label "$label"

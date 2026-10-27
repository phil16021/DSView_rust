#!/bin/bash
# Rebuilds assets/fonts/NotoSansCJKsc-DSViewR-subset.otf, the CJK font built
# into DSViewR (crates/dsview-gui/src/i18n.rs, BUNDLED_CJK_FONT), from Noto
# Sans CJK SC (face 2 of NotoSansCJK-Regular.ttc, Ubuntu's fonts-noto-cjk
# 1:20230817): the CJK characters of assets/lang/cn, 中文, the 3755 hanzi of
# GB 2312 level 1, CJK punctuation and fullwidth forms. Needs fonttools.
#
# usage: tools/fonts/make-cjk-subset.sh [NotoSansCJK-Regular.ttc]
set -euo pipefail
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
TTC="${1:-/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc}"
CHARS="$(mktemp)"
trap 'rm -f "$CHARS"' EXIT
python3 - "$REPO/assets/lang/cn" "$CHARS" <<'PY'
import pathlib, sys
chars = set("中文")
for p in pathlib.Path(sys.argv[1]).rglob("*"):
    if p.is_file():
        chars |= set(p.read_text(encoding="utf-8", errors="ignore"))
for hi in range(0xB0, 0xD8):
    for lo in range(0xA1, 0xFF):
        try:
            chars.add(bytes([hi, lo]).decode("gb2312"))
        except UnicodeDecodeError:
            pass
chars |= {chr(c) for c in range(0x3000, 0x3040)} | {chr(c) for c in range(0xFF00, 0xFFF0)}
chars = {c for c in chars if ord(c) >= 0x2000}
pathlib.Path(sys.argv[2]).write_text("".join(sorted(chars)), encoding="utf-8")
PY
pyftsubset "$TTC" --font-number=2 --text-file="$CHARS" \
    --output-file="$REPO/assets/fonts/NotoSansCJKsc-DSViewR-subset.otf" \
    --layout-features='' --no-hinting --name-IDs='*' --name-languages='*'

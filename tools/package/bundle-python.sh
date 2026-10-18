#!/bin/bash
# Bundles the Python that BIN links into PREFIX/lib/dsview, so the package
# does not depend on the host's Python (dsview_decode::python finds it):
#   PREFIX/lib/dsview/libpython3.X.so.1.0
#   PREFIX/lib/dsview/python3.X/            standard library
#   PREFIX/lib/dsview/python3.X/lib-dynload extension modules
# The Rust binaries carry the RUNPATH $ORIGIN/../lib/dsview already.
#
# usage: tools/package/bundle-python.sh PREFIX BIN [PYTHON]
set -euo pipefail
PREFIX="$1"
BIN="$2"
PYTHON="${3:-python3}"

PYVER="$("$PYTHON" -c 'import sys; print("%d.%d" % sys.version_info[:2])')"
STDLIB="$("$PYTHON" -c 'import sysconfig; print(sysconfig.get_path("stdlib"))')"
PYSO="$(ldd "$BIN" | awk '/libpython/ {print $3}')"
if [ -z "$PYSO" ] || [ ! -f "$PYSO" ]; then
    echo "bundle-python: $BIN does not resolve a libpython (set LD_LIBRARY_PATH)" >&2
    exit 1
fi
case "$PYSO" in
    *libpython"$PYVER".so*) ;;
    *) echo "bundle-python: $BIN links $PYSO, not Python $PYVER" >&2; exit 1 ;;
esac

DEST="$PREFIX/lib/dsview"
mkdir -p "$DEST"
cp -L "$PYSO" "$DEST/"
rsync -a \
    --exclude 'test' --exclude 'idlelib' --exclude 'tkinter' --exclude 'turtledemo' \
    --exclude '__pycache__' --exclude 'site-packages' --exclude 'dist-packages' \
    --exclude 'config-*' \
    "$STDLIB/" "$DEST/python$PYVER/"
echo "bundled Python $PYVER from $STDLIB into $DEST ($(du -sh "$DEST" | cut -f1))"

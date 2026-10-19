#!/bin/bash
# Makes the .deb that cargo-deb built carry its own Python, so it does not
# depend on the host's Python version:
# - the Python that DSViewR links goes to /usr/lib/dsview (bundle-python.sh);
# - DSViewR and dsviewr-cli find it through their RUNPATH ($ORIGIN/../lib/dsview)
#   and dsview_decode::python;
# - the libpython dependency dpkg-shlibdeps found is dropped from the control file.
#
# usage: tools/package/deb.sh DEB [PYTHON]
set -euo pipefail
DEB="$(realpath "$1")"
PYTHON="${2:-python3}"
HERE="$(cd "$(dirname "$0")" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
ROOT="$WORK/root"
dpkg-deb -R "$DEB" "$ROOT"

PYVER="$("$PYTHON" -c 'import sys; print("%d.%d" % sys.version_info[:2])')"
"$HERE/bundle-python.sh" "$ROOT/usr" "$ROOT/usr/bin/DSViewR" "$PYTHON"

# the dependency may come first, last or in the middle of the list
PV="${PYVER//./\\.}"
sed -E -i \
    -e "s/, libpython${PV}[^,]*//g" \
    -e "s/libpython${PV}[^,]*, //g" \
    -e "s/libpython${PV}[^,]*\$//g" \
    "$ROOT/DEBIAN/control"
# Installed-Size grows by the bundle
SIZE="$(du -sk --exclude=DEBIAN "$ROOT" | cut -f1)"
sed -i "s/^Installed-Size: .*/Installed-Size: $SIZE/" "$ROOT/DEBIAN/control"
echo "--- control ---"
cat "$ROOT/DEBIAN/control"
echo "--- libpython of the binaries ---"
for b in "$ROOT/usr/bin/DSViewR" "$ROOT/usr/bin/dsviewr-cli"; do
    echo "$(basename "$b"): $(ldd "$b" | awk '/libpython/ {print $3}')"
done

dpkg-deb --root-owner-group -b "$ROOT" "$DEB"

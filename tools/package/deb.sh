#!/bin/bash
# Makes the .deb that cpack built carry its own Python (release R1), so it
# does not depend on the host's Python version:
# - the Python that DSView links goes to /usr/lib/dsview (bundle-python.sh);
# - the Rust GUI (DSView) finds it through its RUNPATH and
#   dsview_decode::python;
# - the Qt GUI (DSView-qt) gets that RUNPATH and a wrapper script setting
#   PYTHONHOME and PYTHONPATH, as the Qt packages had;
# - the libpython dependency shlibdeps found is dropped from the control file.
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
"$HERE/bundle-python.sh" "$ROOT/usr" "$ROOT/usr/bin/DSView" "$PYTHON"

QT="$ROOT/usr/bin/DSView-qt"
if [ -f "$QT" ]; then
    # shellcheck disable=SC2016 # $ORIGIN is for the dynamic loader
    patchelf --set-rpath '$ORIGIN/../lib/dsview' "$QT"
    mv "$QT" "$QT.bin"
    cat > "$QT" <<WRAPPER
#!/bin/bash
export PYTHONHOME="/usr/lib/dsview"
export PYTHONPATH="/usr/lib/dsview/python${PYVER}:/usr/lib/dsview/python${PYVER}/lib-dynload"
exec "/usr/bin/DSView-qt.bin" "\$@"
WRAPPER
    chmod 755 "$QT"
fi

# the dependency may come first, last or in the middle of the list
PV="${PYVER//./\\.}"
sed -E -i \
    -e "s/, libpython${PV}[^,]*//g" \
    -e "s/libpython${PV}[^,]*, //g" \
    -e "s/libpython${PV}[^,]*\$//g" \
    "$ROOT/DEBIAN/control"
echo "--- control ---"
cat "$ROOT/DEBIAN/control"
echo "--- libpython of the binaries ---"
for b in "$ROOT/usr/bin/DSView" "$ROOT/usr/bin/dsview-cli" "$QT.bin"; do
    if [ -f "$b" ]; then
        echo "$(basename "$b"): $(ldd "$b" | awk '/libpython/ {print $3}')"
    fi
done

dpkg-deb --root-owner-group -b "$ROOT" "$DEB"

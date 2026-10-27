#!/bin/bash
# Finishes the AppDir after linuxdeploy: bundles the Python that DSViewR
# links into usr/lib/dsview (bundle-python.sh) and writes the AppRun.
#
# usage: tools/package/appdir.sh APPDIR [PYTHON]
set -euo pipefail
APPDIR="$(realpath "$1")"
PYTHON="${2:-python3}"
HERE="$(cd "$(dirname "$0")" && pwd)"

"$HERE/bundle-python.sh" "$APPDIR/usr" "$APPDIR/usr/bin/DSViewR" "$PYTHON"

# The library is ABI-stable and on every desktop; an old bundled copy cannot
# parse a newer system's Compose file ("unrecognized keysym").
rm -f "$APPDIR"/usr/lib/libxkbcommon.so* "$APPDIR"/usr/lib/libxkbcommon-x11.so*

rm -f "$APPDIR/AppRun"
cat > "$APPDIR/AppRun" <<'APPRUN'
#!/bin/bash
HERE="$(dirname "$(readlink -f "$0")")"
exec "$HERE/usr/bin/DSViewR" "$@"
APPRUN
chmod 755 "$APPDIR/AppRun"

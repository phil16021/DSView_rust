#!/bin/bash
# Finishes the AppDir after linuxdeploy (release R1): bundles the Python
# that DSView links into usr/lib/dsview (bundle-python.sh) and writes the
# AppRun. The AppImage starts the Rust GUI; `--qt` as the first argument
# starts the Qt GUI, kept for one release, with PYTHONHOME pointing at the
# bundled Python.
#
# usage: tools/package/appdir.sh APPDIR [PYTHON]
set -euo pipefail
APPDIR="$(realpath "$1")"
PYTHON="${2:-python3}"
HERE="$(cd "$(dirname "$0")" && pwd)"

PYVER="$("$PYTHON" -c 'import sys; print("%d.%d" % sys.version_info[:2])')"
"$HERE/bundle-python.sh" "$APPDIR/usr" "$APPDIR/usr/bin/DSView" "$PYTHON"

# linuxdeploy-plugin-qt bundles the build system's libxkbcommon for Qt's xcb
# plugin; the Rust GUI would dlopen it too. An old one cannot parse a newer
# system's Compose file ("unrecognized keysym"), and the library is ABI-stable
# and on every desktop, so both GUIs take the system's.
rm -f "$APPDIR"/usr/lib/libxkbcommon.so* "$APPDIR"/usr/lib/libxkbcommon-x11.so*

rm -f "$APPDIR/AppRun"
cat > "$APPDIR/AppRun" <<APPRUN
#!/bin/bash
HERE="\$(dirname "\$(readlink -f "\$0")")"
# environment hooks of linuxdeploy's plugins
for hook in "\$HERE"/apprun-hooks/*.sh; do
    [ -f "\$hook" ] && . "\$hook"
done
if [ "\${1:-}" = "--qt" ]; then
    shift
    export PYTHONHOME="\$HERE/usr/lib/dsview"
    export PYTHONPATH="\$HERE/usr/lib/dsview/python${PYVER}:\$HERE/usr/lib/dsview/python${PYVER}/lib-dynload"
    exec "\$HERE/usr/bin/DSView-qt" "\$@"
fi
exec "\$HERE/usr/bin/DSView" "\$@"
APPRUN
chmod 755 "$APPDIR/AppRun"

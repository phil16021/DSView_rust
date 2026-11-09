#!/bin/bash
# The nightly hardware run (M7b-PLAN B6.3) on the machine the DSLogic and
# the NUCLEO pattern generator hang on: hwmatrix --quick, verify-counter
# and, when the generator answers on its VCP, hwsig. Each run gets its own
# HOME and XDG directories, so the user's DSView settings stay untouched.
#
# usage: tools/hw/nightly.sh [OUT_DIR]     (exit 0 when everything passed)
#
# Environment: DSVIEW_PYLIB, the directory of the libpython DSViewR links
# (default: the pyenv 3.12.10 one); PATGEN, the generator's VCP
# (default /dev/ttyACM0).
set -uo pipefail
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="${1:-$REPO/target/hw-nightly}"
PYLIB="${DSVIEW_PYLIB:-$HOME/.pyenv/versions/3.12.10/lib}"
PATGEN="${PATGEN:-/dev/ttyACM0}"
export PATH="$HOME/.cargo/bin:$PATH"
mkdir -p "$OUT"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

cd "$REPO" || exit 2
if ! cargo build --release -p dsview-cli > "$OUT/build.log" 2>&1; then
    tail -n 30 "$OUT/build.log"
    echo "nightly: build failed"
    exit 2
fi
# a copy, so a rebuild during the run cannot swap the binary
BIN="$WORK/dsviewr-cli"
cp target/release/dsview-cli "$BIN"

run() { # NAME ARGS...: one step in its own environment, its log in OUT
    local name="$1"
    shift
    mkdir -p "$WORK/$name"/{home,config,data}
    (
        cd "$WORK/$name" &&
            env -u DISPLAY -u WAYLAND_DISPLAY HOME="$WORK/$name/home" \
                XDG_CONFIG_HOME="$WORK/$name/config" XDG_DATA_HOME="$WORK/$name/data" \
                LD_LIBRARY_PATH="$PYLIB" "$BIN" "$@" --data-dir "$REPO/assets"
    ) > "$OUT/$name.log" 2>&1
}

if ! lsusb -d 2a0e: > /dev/null 2>&1; then
    echo "nightly: no DreamSourceLab device on USB"
    exit 3
fi

fail=0
run hwmatrix hwmatrix --quick --out "$OUT/hwmatrix.json" || fail=1
grep -E "^hwmatrix:" "$OUT/hwmatrix.log" || echo "hwmatrix: no summary"
run verify-counter verify-counter --device dslogic || fail=1
grep -E "counter check" "$OUT/verify-counter.log" || echo "verify-counter: no result"
if [ -c "$PATGEN" ]; then
    run hwsig hwsig --patgen "$PATGEN" --work "$WORK/hwsig-work" --out "$OUT/hwsig.json" || fail=1
    grep -E "^hwsig:" "$OUT/hwsig.log" || echo "hwsig: no summary"
else
    echo "hwsig: skipped, no pattern generator at $PATGEN"
fi
[ "$fail" = 0 ] && echo "nightly: all passed" || echo "nightly: FAILED (logs in $OUT)"
exit "$fail"

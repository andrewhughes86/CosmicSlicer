#!/usr/bin/env bash
# Parity check against OrcaSlicer (ROADMAP session 19).
#
#   tools/parity.sh <work dir> [profile set ...]      (sets: x2d x1c mk4 voron; default x2d)
#
# Writes the reference models to <work dir>/models (generated ones from parity_models.py, and
# Orca's own handy models converted from Draco by Orca itself), slices each with OrcaSlicer's
# command line into <work dir>/<model>@<set>/orca.gcode, then runs the engine's parity test,
# which slices the same model with the settings Orca wrote into its G-code and compares:
# <case>/report.txt per case and <work dir>/summary.txt for all.
#
# Orca's command line takes `inherits` literally, so each preset is first flattened from Orca's
# own profile files by parity_flatten.py (as Orca's GUI resolves them).
#
# ORCA overrides how OrcaSlicer is run (default: the Flathub flatpak) and ORCA_SHARE where its
# share/OrcaSlicer folder is (both as that command and as this script sees it, unless
# ORCA_SHARE_HOST gives the latter); MODELS picks models (names, space separated); SKIP_ORCA=1
# reuses the Orca G-code already there.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
work=$(realpath -m "${1:?usage: tools/parity.sh <work dir> [profile set ...]}")
shift
sets=("$@")
[ ${#sets[@]} -eq 0 ] && sets=(x2d)
mkdir -p "$work/models"

if [ -n "${ORCA:-}" ]; then
    orca=$ORCA
    share=${ORCA_SHARE:?set ORCA_SHARE with ORCA}
    share_host=${ORCA_SHARE_HOST:-$share}
else
    orca="flatpak run --filesystem=$work --command=orca-slicer com.orcaslicer.OrcaSlicer"
    share=/app/share/OrcaSlicer
    share_host=$(flatpak info --show-location com.orcaslicer.OrcaSlicer)/files/share/OrcaSlicer
fi

profile_set() {
    case "$1" in
        x2d) echo "BBL/machine/Bambu Lab X2D 0.4 nozzle.json|BBL/process/0.20mm Standard @BBL X2D.json|BBL/filament/Bambu PLA Basic @BBL X2D 0.4 nozzle.json" ;;
        x1c) echo "BBL/machine/Bambu Lab X1 Carbon 0.4 nozzle.json|BBL/process/0.20mm Standard @BBL X1C.json|BBL/filament/Bambu PLA Basic @BBL X1C.json" ;;
        mk4) echo "Prusa/machine/Prusa MK4 0.4 nozzle.json|Prusa/process/0.20mm Standard @MK4.json|Prusa/filament/Prusa Generic PLA @MK4.json" ;;
        voron) echo "Voron/machine/Voron 2.4 350 0.4 nozzle.json|Voron/process/0.20mm Standard @Voron.json|OrcaFilamentLibrary/filament/Generic PLA @System.json" ;;
        *) echo "unknown profile set $1" >&2; return 1 ;;
    esac
}

python3 "$here/parity_models.py" "$work/models" >/dev/null
for m in 3DBenchy Voron_Design_Cube_v7 ksr_fdmtest_v4 OrcaCube_v2 calicat Stanford_Bunny; do
    [ -s "$work/models/$m.stl" ] && continue
    tmp="$work/models/.convert"
    rm -rf "$tmp" && mkdir -p "$tmp"
    if $orca --export-stl --outputdir "$tmp" "$share/handy_models/$m.drc" >/dev/null 2>&1; then
        find "$tmp" -name '*.stl' -exec mv {} "$work/models/$m.stl" \; -quit
    fi
    rm -rf "$tmp"
    [ -s "$work/models/$m.stl" ] || echo "could not convert $m.drc" >&2
done

models=${MODELS:-$(cd "$work/models" && ls *.stl | sed 's/\.stl$//')}
for set in "${sets[@]}"; do
    IFS='|' read -r machine process filament < <(profile_set "$set")
    flat="$work/profiles/$set"
    mkdir -p "$flat"
    for kind in machine process filament; do
        python3 "$here/parity_flatten.py" "$share_host/profiles" "${!kind}" "$flat/$kind.json"
    done
    for m in $models; do
        [ -f "$work/models/$m.stl" ] || continue
        case_dir="$work/$m@$set"
        mkdir -p "$case_dir"
        cp "$work/models/$m.stl" "$case_dir/"
        # Arrange turns a model centred on its origin (the cylinder by 145 degrees, whatever
        # --orient says), and then no vertex matches: such a model is placed on the bed instead.
        arrange=1
        if [ "$m" = cylinder ]; then
            arrange=0
            python3 -I - "$case_dir/$m.stl" <<'PY'
import struct, sys
d = bytearray(open(sys.argv[1], 'rb').read())
for i in range(struct.unpack('<I', d[80:84])[0]):
    o = 84 + 50 * i + 12
    for k in range(3):
        x, y, z = struct.unpack('<3f', d[o + 12 * k:o + 12 * k + 12])
        d[o + 12 * k:o + 12 * k + 12] = struct.pack('<3f', x + 128, y + 128, z)
open(sys.argv[1], 'wb').write(d)
PY
        fi
        [ "${SKIP_ORCA:-}" = 1 ] && [ -s "$case_dir/orca.gcode" ] && continue
        rm -f "$case_dir/orca.gcode" "$case_dir/plate_1.gcode"
        started=$(date +%s.%N)
        if $orca --slice 0 --orient 0 --arrange $arrange --outputdir "$case_dir" \
            --load-settings "$flat/machine.json;$flat/process.json" \
            --load-filaments "$flat/filament.json" "$case_dir/$m.stl" >"$case_dir/orca.log" 2>&1 \
            && [ -s "$case_dir/plate_1.gcode" ]; then
            mv "$case_dir/plate_1.gcode" "$case_dir/orca.gcode"
            printf '%s: Orca sliced in %.1f s\n' "$m@$set" "$(echo "$(date +%s.%N) - $started" | bc)"
        else
            echo "$m@$set: Orca failed (see $case_dir/orca.log)" >&2
        fi
    done
    # Two colours (printers with a filament changer): the cube on filament 1, the cylinder on 2.
    if [ "$set" = x1c ] && { [ -z "${MODELS:-}" ] || [[ " $MODELS " == *" duo "* ]]; }; then
        case_dir="$work/duo@$set"
        mkdir -p "$case_dir"
        cp "$work/models/cube20.stl" "$case_dir/a_cube20.stl"
        cp "$work/models/cylinder.stl" "$case_dir/b_cylinder.stl"
        # A second filament under its own name and colour (Orca 2.4.2 crashes when the same
        # preset is loaded twice).
        python3 -I -c 'import json,sys; d=json.load(open(sys.argv[1])); d["name"] += " B"; d["filament_colour"] = ["#2060E0"]; json.dump(d, open(sys.argv[2], "w"))' \
            "$flat/filament.json" "$flat/filament2.json"
        if [ "${SKIP_ORCA:-}" != 1 ] || [ ! -s "$case_dir/orca.gcode" ]; then
            rm -f "$case_dir/orca.gcode" "$case_dir/plate_1.gcode"
            if $orca --slice 0 --orient 0 --arrange 1 --outputdir "$case_dir" \
                --load-settings "$flat/machine.json;$flat/process.json" \
                --load-filaments "$flat/filament.json;$flat/filament2.json" --load-filament-ids "1,2" \
                "$case_dir/a_cube20.stl" "$case_dir/b_cylinder.stl" >"$case_dir/orca.log" 2>&1 \
                && [ -s "$case_dir/plate_1.gcode" ]; then
                mv "$case_dir/plate_1.gcode" "$case_dir/orca.gcode"
                echo "duo@$set: Orca sliced"
            else
                echo "duo@$set: Orca failed (see $case_dir/orca.log)" >&2
            fi
        fi
    fi
done

cd "$here/.."
PARITY_DIR="$work" "$here/capped.sh" cargo test --release -j 1 -p CosmicSlicer-engine --lib parity_with_orca \
    -- --ignored --nocapture
cat "$work/summary.txt"

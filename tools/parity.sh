#!/usr/bin/env bash
# Parity check against OrcaSlicer (ROADMAP session 19).
#
#   tools/parity.sh <work dir> [profile set ...]      (sets: x1c mk4 voron; default all)
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
[ ${#sets[@]} -eq 0 ] && sets=(x1c mk4 voron)
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
        case_dir="$work/$m@$set"
        mkdir -p "$case_dir"
        cp "$work/models/$m.stl" "$case_dir/"
        [ "${SKIP_ORCA:-}" = 1 ] && [ -s "$case_dir/orca.gcode" ] && continue
        rm -f "$case_dir/orca.gcode" "$case_dir/plate_1.gcode"
        started=$(date +%s.%N)
        if $orca --slice 0 --arrange 1 --outputdir "$case_dir" \
            --load-settings "$flat/machine.json;$flat/process.json" \
            --load-filaments "$flat/filament.json" "$case_dir/$m.stl" >"$case_dir/orca.log" 2>&1 \
            && [ -s "$case_dir/plate_1.gcode" ]; then
            mv "$case_dir/plate_1.gcode" "$case_dir/orca.gcode"
            printf '%s: Orca sliced in %.1f s\n' "$m@$set" "$(echo "$(date +%s.%N) - $started" | bc)"
        else
            echo "$m@$set: Orca failed (see $case_dir/orca.log)" >&2
        fi
    done
done

cd "$here/.."
PARITY_DIR="$work" "$here/capped.sh" cargo test --release -j 1 -p CosmicSlicer-engine --lib parity_with_orca \
    -- --ignored --nocapture
cat "$work/summary.txt"

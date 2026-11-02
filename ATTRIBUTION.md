# Attribution

## OrcaSlicer
CosmicSlicer rebuilds OrcaSlicer (https://github.com/SoftFever/OrcaSlicer), © SoftFever and
contributors, itself based on Bambu Studio, PrusaSlicer and Slic3r. Licensed under the GNU
Affero General Public License v3.

Taken from OrcaSlicer (commit 95a27321456e73cf6c6270d37dccfe819a96b130, 2026-11-01):
- `assets/icons/orca/*.svg` — icons from `resources/images/`.
- `assets/settings/orca_settings.json` — option labels, tooltips, units, ranges, defaults and
  the settings page / group layout, extracted from `src/libslic3r/PrintConfig.cpp` and
  `src/slic3r/GUI/Tab.cpp`.
- The preview legend's feature colours, from `src/libvgcode/src/ViewerImpl.cpp`.
- `assets/calib/` — OrcaSlicer's calibration models (`resources/calib/`: temperature,
  retraction, PA, VFA, max volumetric speed, input shaping and cornering towers, decoded from
  Draco to compressed STL by `tools/pack_calib_models.py`, and the flow-rate test projects).
  The calibration tests follow `src/libslic3r/calib.cpp`, `src/slic3r/GUI/Plater.cpp`
  (`calib_*`), `src/slic3r/GUI/calib_dlg.cpp` and the per-layer changes in
  `src/libslic3r/GCode.cpp`.
- `assets/profiles/orca_profiles.bin` — OrcaSlicer's printer, filament and process profile
  library (`resources/profiles`, 72 vendors), packed by `tools/pack_orca_profiles.py`. The
  vendor profiles were contributed to OrcaSlicer by the printer and filament makers and the
  OrcaSlicer community.
- The custom G-code macro language follows PrusaSlicer's PlaceholderParser as OrcaSlicer uses it;
  the list of template variables follows OrcaSlicer's `PrintConfig.cpp`.

## CosmicSlicer's own
- `assets/icons/app/cosmicslicer.svg` and `assets/icons/cosmic/cosmic_logo.svg` — original.
- The theme palettes and UI kit follow CosmicDMS / CosmicCut by the same author.
- `crates/import` — the model readers (STL, OBJ, PLY, OFF, 3MF, AMF, glTF, COLLADA, VRML, X3D,
  3DS, DXF, STEP, IGES, ACIS SAT) and the B-rep / NURBS / trimmed-face kernel STEP, IGES and
  SAT are tessellated with, from CosmicDMS (`crates/io`, and `brep`, `nurbs`, `trim`, `mesh`
  of `crates/geom`) by the same author, there MIT OR Apache-2.0, here under the AGPL with
  the rest of CosmicSlicer.

## Printer protocols
- Bambu Lab printers in LAN mode (`crates/connect/src/bambu.rs`: MQTT reports and commands,
  implicit FTPS uploads, the sliced `.gcode.3mf`, the P1/A1 camera on port 6000) are written
  from the community's public descriptions of the protocol (OpenBambuAPI); no Bambu code or
  network plugin is used.
- The print hosts follow the APIs OctoPrint, Moonraker, PrusaLink, Duet (RepRapFirmware),
  Repetier-Server, ESP3D, MKS and FlashAir publish, as OrcaSlicer's `src/slic3r/Utils/` uses them.

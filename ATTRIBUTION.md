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

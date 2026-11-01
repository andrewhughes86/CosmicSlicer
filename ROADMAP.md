# CosmicSlicer roadmap

From where CosmicSlicer is (1 November 2026) to a complete OrcaSlicer replacement, one working
session per step. Each step leaves the app releasable: every rule in `AGENTS.md` holds at the
end of every session (no crashes, tests and clippy clean, smoke tests cover new UI).

## Done
| # | Session | Result |
|---|---------|--------|
| 1 | User interface | Every Orca page, sidebar, all 580 settings in Orca's layout, 3D plates, Move/Rotate/Scale/Lay on face, STL/OBJ import |
| 2 | Slicing engine core | Layers, walls, top/bottom/solid/sparse, 7 infill generators, skirt/brim, normal supports, cooling, G-code, estimates, real toolpath Preview, export |
| 3 | CI, autosave, crash recovery | GitHub Actions on Linux/Windows/macOS (+ real app run on a software GPU), autosave with OS-lock crash detection and restore prompt, crash reports, atomic settings files, shader validation test |
| 4 | Profiles | OrcaSlicer's library (72 vendors, 1,192 printers) embedded, setup wizard, compatibility filtering, printer defaults, the custom G-code macro language (all 4,768 library templates render), user presets as Orca-style files, import/export |
| 5 | Motion and G-code | Firmware-style motion planner (estimate and M73), machine limits, per-feature accel/jerk, pressure advance, firmware retraction, wipe, sloped z-hop, travel inside parts and avoid-crossing-walls routing, arc fitting, object labels / exclude-object, thumbnails, G-code viewer with travel/retraction/seam marks |
| 6 | Walls | Variable-width thin walls and gap fill (medial axis), precise outer wall, wall direction, overhang degrees with Orca's speeds/fan/flow, make overhangs printable, bridge direction detection, internal and thick bridges, corner-aware seams, scarf seams, seam gap, staggered inner seams, role-based wipe, wipe on loops |

## Order, and why
Get a user's own printer printing well and safely first (profiles, correct motion G-code,
saving work), then print quality (walls, infill, supports), then the model-editing features,
then multi-material and connectivity, then completeness, hardening and release. Cross-platform
CI comes early because the Windows and macOS builds are not yet checked by anything.

| # | Session | Depends on |
|---|---------|------------|
| 3 | Repository, CI on Windows/macOS/Linux, autosave and crash recovery | — |
| 4 | Printer, filament and process profiles (Orca's library, wizard, user presets) | 3 |
| 5 | Motion and G-code correctness, accurate time estimate, G-code viewer | 4 |
| 6 | Walls: Arachne, gap fill, thin walls, overhangs, bridges, seams | 2 |
| 7 | Infill and surfaces: every Orca pattern and option | 6 |
| 8 | Supports: tree/organic, raft, brim ears, enforcers/blockers in the engine | 6 |
| 9 | Project files: Orca-compatible 3MF, STEP/AMF/3MF/SVG import | 4 |
| 10 | Object model: parts, modifiers, negative volumes, per-part and per-range settings | 9 |
| 11 | Prepare tools: cut, text, SVG, measure, assemble, painting brushes | 10 |
| 12 | Multi-material: prime tower, flushing, colour painting, sequential printing | 10, 11 |
| 13 | Layer height: variable and adaptive layers, spiral vase, by-object printing | 6 |
| 14 | Arrange, orient and plates done properly | 10 |
| 15 | Printer connections and the live Device / Multi-device pages | 5 |
| 16 | Calibration suite | 5, 7 |
| 17 | App completeness: preferences, translations, shortcuts, Home, 3D mouse | — |
| 18 | Hardening: fuzzing, performance, memory, soak tests | all engine work |
| 19 | Parity check against OrcaSlicer on a reference set | 6–14 |
| 20 | Packaging and release for all three platforms | 3, 18 |

17 more sessions. Sessions 6–8 and 13 can swap order; 17 can be split up and done between others.

---

## 3 · Repository, CI, autosave and crash recovery — done
- CI on GitHub Actions for Linux, Windows and macOS: fmt, clippy, tests, release build;
  artefacts per platform.
- Headless smoke tests run on all three (egui without a GPU), plus a wgpu software-adapter test
  where the runner allows it.
- Autosave the open project every few minutes and on every slice; offer to restore it after an
  unclean exit. A panic hook writes a crash report (stack, version, OS) next to the autosave.
- Settings, presets and recent files saved atomically (write then rename), so a crash mid-save
  never corrupts them.

**Done when:** CI is green on all three; killing the app mid-edit and reopening it restores the plate.

## 4 · Profiles — done
- Read Orca's `resources/profiles` (vendors, printer models, variants, filaments, processes)
  with `inherits`, `compatible_printers` / `compatible_printers_condition` and per-nozzle variants.
- The setup wizard: pick printers and filaments by vendor, as Orca's first-run guide does.
- Preset combos show only compatible filaments and processes for the selected printer.
- User presets stored as Orca-format JSON under the config folder; import/export configs and
  preset bundles; rename, delete, detach from parent; "transfer or discard" kept.
- Printer settings that the engine ignores today are listed in a test so none are forgotten.

**Done when:** your own printers come up with their real start/end G-code, bed shape and limits,
and an Orca user preset file imports and slices identically.

## 5 · Motion and G-code — done
- Machine limits (`machine_max_*`) emitted as M201/M203/M204/M205 or Klipper `SET_VELOCITY_LIMIT`;
  per-feature acceleration and jerk commands; pressure advance / adaptive PA.
- Time estimate rebuilt on Orca's GCodeProcessor model (trapezoids with look-ahead, junction
  deviation, machine limits): within a few percent of Orca and of the real print.
- Avoid crossing walls (travel routed inside the part), retract only when leaving it, wipe on
  retract, firmware retraction, z-hop types (normal, slope, spiral) and "lift only above/below".
- Arc fitting (G2/G3), Klipper `EXCLUDE_OBJECT` labels, thumbnails in the G-code (PNG / QOI /
  BTT formats), M73 progress, G-code flavours (Marlin, Klipper, RepRap, Repetier).
- Open an existing `.gcode` file in Preview (G-code viewer mode) with the G-code window beside it.
- Preview: travels, retractions, seams and wipes as Orca's option toggles; tool marker on the
  horizontal slider.

**Done when:** a test print on your printer runs at the expected time, and G-code from Orca and
from CosmicSlicer for the same model load into the viewer with matching statistics.

## 6 · Walls — done
- Thin walls and gap fill: narrow parts and gaps between walls are found by opening the region
  and filled with variable-width centre lines (distance transform + thinning), split into
  constant-width runs. Arachne prints these as inner walls, classic as gap fill; thin walls
  follow `detect_thin_wall` (always on with Arachne), `min_bead_width`, `min_feature_size`,
  `filter_out_gap_fill`, `gap_fill_flow_ratio`. Gap fill in solid surfaces by `gap_fill_target`.
- Precise outer wall, wall direction (holes the other way), only one wall on the first layer,
  "detect narrow internal solid infill" (concentric).
- Overhangs: each wall segment's degree (10/25/50/75/95 % of the line over air) picks
  `overhang_1_4_speed`…`overhang_4_4_speed` or the bridge speed, `overhang_flow_ratio`, and the
  overhang fan by `overhang_fan_threshold`; 50 %+ prints as "Overhang wall". "Make overhangs
  printable" fills steep overhangs out into cones.
- Bridges: the direction best anchored at both ends (or `bridge_angle`), bridge density, thick
  bridges; solid infill over sparse infill becomes an "Internal Bridge" with its own angle,
  density, speed, flow, thickness and fan.
- Seams: aligned and nearest seams prefer concave corners, then convex ones, and avoid
  overhangs; seam gap; staggered inner seams; scarf joint seams (external / all, conditional on
  the corner angle, start height, steps, length or entire loop, inner walls, speed and flow);
  role-based wipe speed and `wipe_speed`; wipe on loops.
- The G-code viewer splits layers at `;LAYER_CHANGE` when a file has them, so scarf ramps and
  z-hops stay in their layer (Orca's G-code too).
- Checked on Orca's KSR FDM test and 3DBenchy (decoded from its `handy_models`): both slice and
  read back with the same layers, time and filament.

Moved on: extra perimeters on overhangs (session 7), Arachne's wall-count transitions in parts
that are wide enough for walls (session 19; thin parts and gaps already get variable widths),
`make_overhang_printable_hole_size` and `scarf_overhang_threshold` (session 19).

## 7 · Infill and surfaces
- Extra perimeters on overhangs (from session 6).
- The remaining patterns natively: honeycomb, 3D honeycomb, lightning, adaptive cubic, support
  cubic, quarter cubic, cross hatch, Hilbert curve, Archimedean chords, octagram spiral, TPMS,
  lateral lattice / honeycomb, zig-zag variants, monotonic line.
- Infill anchors and anchor length, infill combination, sparse infill rotation template,
  filter out small internal areas, ensure vertical shell thickness, top surface flow / density,
  solid infill threshold area.
- Ironing types (top, topmost, all solid) with pattern, spacing, inset and angle; fuzzy skin.
- Gyroid made fast (wave generation instead of marching squares).

**Done when:** every pattern in the menu is its own generator, and the 300 mm gyroid sample slices
in under 20 s.

## 8 · Supports
- Tree supports (Orca's organic and slim/strong/hybrid), snug and grid normal styles.
- Rafts (layers, contact distance, first-layer expansion), support on build plate only,
  bottom interface layers, independent support layer height.
- Brim ears and painted brims; brim per object; brim/skirt per object in by-object printing.
- Engine side of support enforcers/blockers and painted support (UI in session 11).

**Done when:** Orca's support test set (arch, T, tree-friendly figurine) prints removable supports.

## 9 · Project files and import
- 3MF read and write compatible with OrcaSlicer and Bambu Studio: plates, objects, parts,
  transforms, per-object and per-plate settings, filament list and colours, painted data,
  thumbnails; open Orca projects and have Orca open ours.
- Import STEP (from CosmicDMS's reader), AMF, colour OBJ, 3MF models from other slicers, ZIP of
  models, SVG to emboss; drag in multiple files; "seems to be in inches" and huge-mesh prompts.
- Recent projects with thumbnails on Home.

**Done when:** a project saved by Orca reopens here with the same plates and settings, and back.

## 10 · Object model
- Objects with parts, modifiers, negative volumes, support enforcers/blockers; instances.
- Orca's object list: tree, per-object / per-part / per-height-range settings, drag to reorder,
  context menus (add part/modifier from file or primitive, split to parts, assemble).
- Mesh repair on import (normals, holes, self-intersections) with a report.
- Mesh boolean (union, difference, intersection) between parts.
- Engine: regions per part and modifier, settings resolved per region and height range.

**Done when:** a modifier cube changing infill inside part of a model slices exactly there.

## 11 · Prepare tools
- Cut: planar cut with connectors (plug, dowel, snap), keep/flip parts, cut to parts.
- Text shape (emboss) and SVG (emboss/engrave), measure (point, edge, plane), assemble.
- Painting brushes with per-triangle state saved in 3MF: support, seam, fuzzy skin, colour
  (circle, sphere, fill, gap fill, smart fill), and the engine consuming them.
- Lay on face with face highlighting, scale to fit, drop to bed per tool.

**Done when:** every toolbar button works; no "arrives later" notes remain in Prepare.

## 12 · Multi-material
- Prime/wipe tower (Orca's), flushing volumes matrix with auto-calculation from colours,
  flush into infill / objects / support, ooze prevention and standby temperatures.
- Colour painting sliced into per-filament regions (MMU segmentation), filament for features.
- Print sequence by object with extruder clearance and collision checks; per-plate sequence.
- Tool change G-code, filament change G-code, pause/colour change at layer from the slider.

**Done when:** a painted two-colour model prints with a tower and correct purges.

## 13 · Layer height
- Variable layer height tool (paint smooth/adaptive), adaptive layer height, layer ranges.
- Proper spiral vase (continuous Z, smoothing), sequential printing heights.

**Done when:** the variable-layer tool's curve slices as drawn and the Preview shows it.

## 14 · Arrange, orient and plates
- Arrangement with rotation and alignment like Orca's (nesting, spacing, avoiding excluded bed
  areas and the wipe tower), arrange selection / all plates, auto-plate new objects.
- Auto orient minimising overhang and support (Orca's method).
- Plates: drag objects between plates, plate thumbnails, per-plate names/types/sequence used by
  the engine, slice all plates, export all plates, plate list strip.

**Done when:** "Arrange all" packs 30 mixed parts like Orca and nothing overlaps.

## 15 · Printer connections and Device pages
- Physical printers with Orca's host types: OctoPrint/Klipper (Moonraker), PrusaLink,
  PrusaConnect, Duet, Repetier, MKS, ESP3D, FlashAir, AstroBox, Obico, SimplyPrint, Flashforge,
  Elegoo; upload, upload-and-print, test connection, storage selection.
- Device page live for Moonraker/OctoPrint/PrusaLink: temperatures, progress, camera (MJPEG/
  WebRTC where possible), pause/stop, jog, fans; Multi-device with several printers.
- Bambu Lab printers on the local network (LAN-only mode), without Bambu's closed network
  plugin: discovery (SSDP), status and control over MQTT/TLS (port 8883, access code), upload
  over FTPS (port 990), start the print with AMS filament mapping, camera stream; Bambu's
  sliced-3MF format (`.gcode.3mf`) with plate G-code and metadata. Newer firmware needs
  "LAN only" / developer mode on the printer, which the connect dialog explains.
- Network code on its own thread with timeouts; a dead printer never blocks the window.
- Not planned: Bambu's cloud service (account login, remote printing), which needs their
  closed plugin.

**Done when:** "Print plate" sends to your Klipper printer and to a Bambu printer in LAN mode,
and the Device page follows both prints.

## 16 · Calibration
- Orca's calibration prints, generated with their settings: temperature tower, flow rate
  (coarse/fine and YOLO), pressure advance (line, pattern, tower), retraction tower, max
  volumetric speed, VFA, input shaping frequency and damping, cornering (jerk/junction).
- Results written back into the filament or printer preset.

**Done when:** every Calibration menu item produces the print and its instructions.

## 17 · App completeness
- Full Preferences (all of Orca's pages that apply), every Orca keyboard shortcut, 3Dconnexion.
- Translations: Orca's `.po` catalogues loaded, language picker, right-to-left check.
- Home: recent projects with thumbnails, manuals and online models links.
- Project page: pictures, BOM, assembly guide, profile notes saved in the 3MF.
- High-DPI and multi-monitor checks; light/dark switching everywhere; accessibility labels.

## 18 · Hardening
- Fuzz the STL/OBJ/3MF/STEP/G-code readers and the engine with random meshes and settings
  (cargo-fuzz plus property tests); every input either slices or gives an error.
- Performance: parallel per-layer everything, memory caps for huge models, Preview level of
  detail; budgets measured on a reference set and checked in CI.
- Soak test: hours of scripted UI actions (load, edit, slice, undo, switch pages) on all three
  platforms with no panic recovered and no memory growth.

## 19 · Parity with OrcaSlicer
- A reference set (calibration cube, Benchy, thin walls, overhang test, multi-part, multi-colour,
  large and small parts) sliced by both with the same profiles.
- Compare time, filament, layer count, per-feature totals and toolpath maps; explain or fix every
  difference over a few percent; keep the set as a regression test.
- Test prints on your printers.
- From session 6: Arachne's wall-count transitions where a part narrows but still has room for
  walls; `make_overhang_printable_hole_size`; `scarf_overhang_threshold`.

## 20 · Packaging and release
- Windows installer (MSI) with file associations; macOS `.app` (universal) as a DMG; Linux
  AppImage and Flatpak (deb optional). Icons, version info, licence and attribution bundled.
- Signed installers: Windows with an Authenticode certificate (timestamped), macOS with a
  Developer ID Application certificate, hardened runtime and notarisation (stapled), Linux
  AppImage with a GPG signature and checksums. Certificates live as CI secrets, never in the
  repository; you provide them (a Windows code-signing certificate and an Apple Developer
  account) before this session.
- Release workflow from a tag; in-app "new version available" notice.

## Decisions
- Repository: github.com/andrewhughes86/CosmicSlicer. Claude commits; you push.
- Bambu Lab printers: supported on the local network (session 15).
- Installers: signed on Windows and macOS, signature and checksums on Linux (session 20). You
  provide the Windows certificate and Apple Developer account before then.

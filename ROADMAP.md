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
| 7 | Infill and surfaces | Every Orca infill pattern as its own generator (incl. lightning, adaptive/support/quarter cubic, 3D and lateral honeycomb, lateral lattice, cross hatch, TPMS-D/FK, Hilbert, Archimedean, octagram, zig-zag variants, monotonic line), gyroid solved per layer as waves, anchors, infill combination, rotation templates, small-area filter, vertical shell thickness, top surface flow/density, extra perimeters on overhangs, ironing, fuzzy skin |
| 8 | Supports | Grid and snug normal supports, tree supports (organic, slim, strong, hybrid) with trunk brims, top and bottom interfaces with every Orca pattern, top/bottom/xy/first-layer gaps, on build plate only, critical regions only, enforce layers, rafts, per-object brims (outer, inner, both, mouse ears), support enforcer/blocker volumes and painted support in the engine |
| 9 | Project files and import | 3MF projects in OrcaSlicer's layout (plates, objects and copies, per-object and per-plate settings, presets by name with edits, painted data, thumbnails), Orca / Bambu / plain 3MF opened; CosmicDMS's readers vendored (STEP, IGES, SAT, AMF, PLY, OFF, glTF, COLLADA, VRML, X3D, 3DS, DXF), zip archives, multi-object 3MF import, inches prompt, recent projects with thumbnails |
| 10 | Object model | Parts, modifiers, negative volumes, support enforcers/blockers and height ranges on objects (model, engine, 3MF, autosave); object list as a tree with Orca's add-part/modifier menus (primitive or file), split to parts, assemble, drag to reorder, per-part and per-range settings; mesh repair on import with a report |

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

10 more sessions. Session 13 can come earlier; 17 can be split up and done between others.

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

## 7 · Infill and surfaces — done
- Extra perimeters on overhangs (from session 6).
- Every pattern in the menu is its own generator: honeycomb, 3D honeycomb, lightning, adaptive
  cubic, support cubic, quarter cubic, cross hatch, Hilbert curve, Archimedean chords, octagram
  spiral, TPMS-D and TPMS-FK (traced on a grid), lateral lattice / honeycomb, zig-zag, cross
  zag, locked zag, monotonic line.
- Gyroid solved per layer as waves (y along x, or x along y), not marching squares.
- Infill anchors and anchor length, infill combination, sparse and solid infill rotation
  templates, filter out small internal areas, ensure vertical shell thickness, top surface flow
  and density, solid infill threshold area.
- Ironing types (top, topmost, all solid) with pattern, spacing, inset and angle; fuzzy skin
  (`fuzzy.rs`).

Not yet verified: the 300 mm gyroid timing. `the_gyroid_is_fast_and_smooth` is `#[ignore]`d
because running it alongside the editor ran a 7 GB machine out of memory; run it with
`cargo test --release -p CosmicSlicer-engine the_gyroid -- --ignored` and check the 300 mm
sample slices in under 20 s (session 18 at the latest).

## 8 · Supports — done
- Normal supports, grid (columns closed into blocks) and snug (following the overhang), from
  the overhang angle or, at 0°, the overlap threshold; critical regions only; remove small
  overhangs; enforce support for the first layers; support expansion and angle.
- Tree supports (`tree.rs`): tips on a plate-fixed grid under each overhang; branches lean by
  the branch angle, slide off the part they would land on (looking up to 30 layers ahead),
  keep clear of it and merge with neighbours, thickening from the tip diameter to the branch
  diameter and then by the diameter angle. Organic, slim, strong (two walls) and hybrid
  (columns under broad flat overhangs); wall count, with infill, trunk brim.
- Top and bottom interface layers with their own spacing and pattern (rectilinear,
  interlaced, grid, concentric, spiral inset); base patterns rectilinear, grid, honeycomb,
  lightning and hollow; top, bottom, XY and first-layer gaps; on build plate only (columns
  never stand on the part; trees that would land on it are dropped whole); support and
  interface flow ratios.
- Rafts: layers, contact distance (the part's first layer then prints at the normal layer
  height), expansion, first-layer expansion and density, dense interface layers on top.
- Brims per object with its own settings, kept off the other objects: outer, inner, outer and
  inner, mouse ears at corners sharper than the ear angle.
- Engine side of enforcers and blockers: volumes attached to an object and painted triangles
  (`JobObject::support_volumes`, `support_paint`); manual support types use only these.
- Checked on an arch, a T and a ball-on-a-stick figure with every style: each slices with
  support and interfaces, and reads back with the same time. (Orca's own support test models
  are not on this machine; printing them is for session 19.)

Moved on: independent support layer height (session 13, with per-object layer stacks),
painted brims (session 11, with the painting tools), leading-edge-only brims and brims per
object in by-object printing (session 13), support ironing and small-perimeter support speed
(session 19).

## 9 · Project files and import — done
- 3MF projects (`model::threemf`, `ui-egui/project.rs`) in OrcaSlicer's layout: each object's
  parts in `3D/Objects/object_N.model` as components of the main object, build items for every
  copy, `Metadata/model_settings.config` (names, filaments, part types, per-object settings,
  plates with their name, lock, plate type, sequence, spiral and objects), Orca's
  `project_settings.config` JSON (every printer, process and filament value, filament values as
  one list entry per filament, preset names, inherits and changed keys), plate thumbnails, and
  the painted facets as Orca's encoded strings (`paint_supports`, `paint_seam`, `paint_color`),
  kept as they are; painted supports reach the engine.
- Opening selects presets by name — installed, then the library — and keeps the project's
  changed values as unsaved edits; a preset found nowhere becomes a project preset.
- Plain 3MF from other programs (inline meshes, nested components, units) opens or imports.
- CosmicDMS's readers vendored as `crates/import`: STL, OBJ, PLY, OFF, 3MF, AMF, glTF,
  COLLADA, VRML, X3D, 3DS, DXF, STEP, IGES and ACIS SAT; zip archives of models; multi-object
  files keep their layout; Open Project / Save / Save As (Ctrl+Shift+S); dropped and recent
  `.3mf` files open as projects; the "seems to be in inches" prompt and a huge-mesh warning;
  recent projects on Home show their plate thumbnail.

Not verified: no OrcaSlicer or Bambu Studio project files were on this machine, so opening
theirs is tested on files written in their layout, not on files they wrote. CosmicSlicer
writes itself as the project's application; OrcaSlicer may take that as a non-Orca 3MF and
load the geometry only. Check both directions with real files (session 19).

Moved on: modifier, negative and support parts (left out with a warning; session 10), colour
OBJ (session 12), SVG import (session 11, with emboss).

## 10 · Object model — done
- Objects carry more volumes (`model::Volume`): parts, modifiers, negative volumes and support
  enforcers/blockers, each with its placement in the object and its own settings, and height
  ranges (`model::HeightRange`). Bounds, arrange, drop to bed, picking and STL export count the
  parts; autosave and 3MF projects keep them (Orca's part subtypes; ranges in
  `Metadata/layer_config_ranges.xml`).
- Engine: parts join the object's slices and negative volumes cut them; modifiers and parts
  with settings split each island's fill into pieces filled by their own settings (the last
  listed wins), and a height range sets its layers' settings whole, walls included
  (`plan::LayerRegions`); enforcer/blocker volumes feed supports.
- Object list as a tree: fold, volumes and ranges under their object with Orca's icons, drag
  to reorder; context menus add a part, negative volume, modifier, support enforcer or blocker
  from a primitive or a file, a height range, split to parts, assemble; a volume's type can be
  changed. Picking a volume or range shows its settings in the Process panel's Objects mode,
  with its position and scale or its heights. Modifiers and the rest draw see-through.
- Mesh repair on import: degenerate triangles, reversed triangles (and inside-out pieces),
  holes up to 500 edges closed; a notice says what was fixed and what stays open.
- `--add-volume kind:primitive` for screenshot runs.

**Done when** is met by the engine test `a_modifier_changes_the_infill_exactly_inside_it`: a 60 %
modifier in a 10 % cube fills its middle 4–9 times as densely while a strip just outside stays
sparse.

Moved on: modifiers and per-part settings change the fill only; wall settings change through
height ranges but not yet inside modifiers (session 19). Volumes move from their panel, not the
canvas gizmos, and those edits are not undoable step by step (session 11, with the gizmo work).
Mesh booleans between parts and repair of self-intersections (session 18). OrcaSlicer's shared
instances: copies are separate objects here (session 14, with plates). Painted facets on volumes
other than the main mesh are not kept (session 11).

## 11 · Prepare tools
- Cut: planar cut with connectors (plug, dowel, snap), keep/flip parts, cut to parts.
- Text shape (emboss) and SVG (emboss/engrave), measure (point, edge, plane), assemble.
- Painting brushes with per-triangle state saved in 3MF: support, seam, fuzzy skin, colour
  (circle, sphere, fill, gap fill, smart fill), and the engine consuming them; painted brims
  (from session 8).
- Lay on face with face highlighting, scale to fit, drop to bed per tool.
- From session 10: move, rotate and scale volumes with the canvas gizmos (undoable), and keep
  painted facets on volumes other than the main mesh.

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
- From session 8: independent support layer height (support on its own layer stack, so the
  top gap need not be a whole number of layers), brims per object in by-object printing,
  leading-edge-only brims.

**Done when:** the variable-layer tool's curve slices as drawn and the Preview shows it.

## 14 · Arrange, orient and plates
- Arrangement with rotation and alignment like Orca's (nesting, spacing, avoiding excluded bed
  areas and the wipe tower), arrange selection / all plates, auto-plate new objects.
- Auto orient minimising overhang and support (Orca's method).
- Plates: drag objects between plates, plate thumbnails, per-plate names/types/sequence used by
  the engine, slice all plates, export all plates, plate list strip.
- From session 10: OrcaSlicer's shared instances (one object, several placements).

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
- From session 10: mesh booleans between parts (union, difference, intersection) and repair
  of self-intersections.

## 19 · Parity with OrcaSlicer
- A reference set (calibration cube, Benchy, thin walls, overhang test, multi-part, multi-colour,
  large and small parts) sliced by both with the same profiles.
- Compare time, filament, layer count, per-feature totals and toolpath maps; explain or fix every
  difference over a few percent; keep the set as a regression test.
- Test prints on your printers.
- From session 6: Arachne's wall-count transitions where a part narrows but still has room for
  walls; `make_overhang_printable_hole_size`; `scarf_overhang_threshold`.
- From session 10: wall settings inside modifiers and parts (Orca's per-region perimeters).

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

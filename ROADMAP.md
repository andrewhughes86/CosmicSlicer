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
| 11 | Prepare tools | Support, seam, fuzzy-skin and colour painting (circle, sphere, fill, smart fill, gap fill) with the engine using support, seam and fuzzy paint; Cut with capped faces, flip, cut to parts, plug and dowel connectors; Text and SVG emboss/engrave; Measure; brim ears placed by hand ("Painted" brim); Boolean union/difference; Assemble; lay-on-face highlight, scale to fit; undoable volume edits |
| 12 | Multi-material | Colour painting sliced into filaments (nearest painted surface, or a set depth); layers printed filament by filament; prime tower sized to the purges, with brim and sparse layers; flushing volumes table (auto from colours, multiplier); change-filament G-code; ooze prevention; support and interface filaments; print by object with clearance checks and per-plate sequence; colour change, pause and custom G-code from the layer slider (3MF `custom_gcode_per_layer.xml`) |
| 13 | Layer height | Variable layer height tool (draw the curve, adaptive from surface slopes with a quality setting, smooth, reset), saved per object (3MF `layer_heights_profile.txt`); layer stacks from profiles and height ranges with `layer_height`; a continuous-Z spiral vase; each object's brim with it when printing one at a time |
| 14 | Arrange, orient and plates | No-fit-polygon arrangement of convex footprints (spacing, auto rotate, excluded bed area, prime tower), arrange selection / plate / all plates, overflow and new objects onto new plates; auto orient by overhang and contact area; shared instances (+ / −, 3MF one object placed many times); prime tower drawn and dragged on the canvas, per plate; slice all, export every plate's G-code, Preview plate strip with thumbnails |
| 15 | Printer connections | Physical printers with Orca's host types; upload / upload-and-print to OctoPrint, Klipper (Moonraker), PrusaLink (digest or key, storage choice), Duet, Repetier, AstroBox, ESP3D, MKS, FlashAir; Bambu Lab printers in LAN mode (MQTT status and control, FTPS upload, `.gcode.3mf`, SSDP discovery, P1/A1 camera); live Device page (state, progress, temperatures, pause/resume/stop, jog, fan, speed, camera) and Multi-device list; every connection on its own thread |
| 16 | Calibration | Every Orca calibration: temperature tower, flow rate (pass 1 and 2, YOLO and perfectionist), pressure advance (line, pattern, tower), retraction tower, max volumetric speed, VFA, input shaping frequency and damping, cornering (jerk / junction deviation); Orca's models, settings and per-layer changes; results saved into the filament or printer preset |
| 17 | App completeness | OrcaSlicer's translations (22 languages, system fonts for CJK and Thai), Preferences pages wired to real behaviour (language, start page, project load behaviour, backup interval, recent files, arrange after clone, remembered print action, camera style, drag buttons, orbit speed, reverse zoom, log level), Orca's keyboard shortcut table, 3Dconnexion through spacenavd, Project page profile notes and attachments in the 3MF, accessibility names, checks at several pixel densities |

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

3 more sessions. 17 can be split up and done between others.

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

Verified in session 18: the 300 mm gyroid fills in 0.39 s (target under 20 s).
`the_gyroid_is_fast_and_smooth` stays `#[ignore]`d; run it with
`tools/capped.sh cargo test -p CosmicSlicer-engine the_gyroid -- --ignored`.

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

## 11 · Prepare tools — done
- Painting (`viewport/tools.rs`): support, seam, fuzzy skin and colour on the object's main
  mesh, whole triangles as OrcaSlicer stores them, with circle and sphere brushes (nearest
  point of each triangle within the radius), fill, smart fill by angle and gap fill; enforce,
  block or erase (a filament for colour); a stroke is one undo step; saved in 3MF
  (`paint_supports`, `paint_seam`, `paint_color`, `paint_fuzzy_skin`) and autosave.
- Engine: painted seams go inside enforcers and never in blockers (`place_seam`); painted
  fuzzy skin roughens the outer walls inside the paint only, even with fuzzy skin off; upright
  painted faces become bands round the walls (`slicing::painted` pad).
- Cut (`model::cut`): a plane at a height with tilt about X and Y, drawn on the canvas; both
  sides closed (holes kept), keep top and/or bottom, turn the top over, or keep both as parts;
  plug connectors (a peg on the top, a hole with clearance below) and dowels (holes both sides
  and a pin object), placed by clicking the plane.
- Text shape: lines of text in the built-in Sans or Mono font or a loaded TTF/OTF, and SVG
  files (`emboss.rs`, `model::shape2d`), as an object or embossed, engraved or as a modifier
  on the selected object's top.
- Measure: distance and ΔX/ΔY/ΔZ between two picks, the angle between their faces and the
  gap between parallel faces.
- Brim ears placed by hand under an object (Shift-click removes), printed with the "Painted"
  brim type; saved in `Metadata/brim_ear_points.txt` (our layout of Orca's file, unverified).
- Boolean: union (assemble as parts) and difference (the others become negative volumes);
  Assemble panel; Split to parts on the toolbar; lay on face highlights the face under the
  mouse; Scale to fit; Drop to bed in the move, rotate and scale panels; volume and height
  range edits are undoable.

Not done: colour painting is drawn and saved but prints in one filament until session 12
slices it into filaments; Boolean intersection needs mesh booleans (session 18); painting
volumes other than the main mesh, and triangles split finer than the mesh's own (session 19);
volumes moved with the canvas gizmos rather than their panel (session 19). Variable layer
height's toolbar button opens a note pointing at session 13.

## 12 · Multi-material — done
- Colour painting sliced into filaments (`segment.rs`): each layer split among the colours
  painted on its surface, every point taking the nearest painted surface's colour, or each
  colour a set depth in (`mmu_segmented_region_max_width`); each part planned on its own, its
  islands marked with their filament.
- Each layer prints filament by filament (support first, with `support_filament` /
  `support_interface_filament` when set; then the loaded filament's islands), one change per
  filament per layer.
- Prime tower: at each change a purge of the flushing volume in rows across the tower; sized
  from the layer with most purging, with a brim on the first layer, walls and sparse rows on
  every layer up to the last change; warnings when it is off the bed or over an object.
- Flushing volumes: the table from `flush_volumes_matrix` (× multiplier), or from the colours
  (more for light after dark, after OrcaSlicer's estimate); an editable dialog with
  auto-calculate, multiplier and the tower switch.
- Change-filament G-code with `previous_extruder` / `next_extruder` (a template's own `T`
  replaces ours); ooze prevention (idle hotend to its standby temperature).
- Print by object: each object to the top, shortest first, with its own supports; up clear of
  what is printed, across, then down; warnings for objects inside the extruder clearance or
  taller than its height; per-plate sequence.
- Colour change (M600), pause (the printer's pause G-code, else M601) and custom G-code from
  the Preview's layer slider, marked on it, saved with the plate (3MF
  `Metadata/custom_gcode_per_layer.xml`, written in Orca's layout as we understand it).
- Checked by engine tests: a cube painted on one face in a second filament prints both, with
  at most two changes a layer, purges in the tower, and reads back with the same time and
  filament per extruder. Not printed on a real multi-material printer.

Moved on: flushing into infill, objects or support instead of the tower, and the wall /
infill filament options (missing from our extracted settings; re-extract them) to session 19;
moving the tower on the canvas to session 14.

## 13 · Layer height — done
- Variable layer height (`model::layers`): a profile of layer height by height above the plate
  per object; the toolbar tool draws it as a curve (drag to set the height under a brush that
  fades at its edge), Adaptive from the surfaces (thin where they lie near flat, thick up walls;
  a quality setting), Smooth (radius, keeping the thinnest layers or not) and Reset; each a
  single undo step; saved in autosave and 3MF (`Metadata/layer_heights_profile.txt`, our
  reading of Orca's layout).
- Engine: the plate's layers follow the thinnest layer any object asks for at each height
  (its profile, or a height range that sets `layer_height`), within the printer's minimum and
  maximum layer height, the last layer ending at the top (`slicing::layer_heights_by`).
- Spiral vase: above the solid bottom, on single-loop layers, the outer wall climbs from the
  last layer's height to this one's as it goes round, carrying on from where the last turn
  ended, its flow growing from nothing on the first turn; no seam, no loop wipe, no arcs there.
- Printing one at a time, each object's brim prints with its first layer.
- Checked by engine tests: a drawn profile slices 0.1 mm layers low and 0.3 mm high, rising
  steadily between, and the Preview's layers show those heights; a spiral vase never moves down
  while printing and reads back with the same time.

Moved on to session 19: per-object layer stacks interleaved by height (here one stack serves
the plate, the thinnest any object wants); independent support layer height; the spiral vase's
XY smoothing between layers (`spiral_mode_smooth`); leading-edge-only brims. The layer height
profile is not drawn beside the object on the canvas, only in the tool's panel.

## 14 · Arrange, orient and plates — done
- Arrange (`model::arrange`): each object's footprint is the convex hull of its shadow, grown
  by half the spacing; largest first, each goes where it touches what is placed (no-fit
  polygons of convex outlines, candidates at their corners and crossings) so the pile stays
  tight round the plate's centre, lined up with its neighbours where that ties; with "Auto
  rotate", also square to the plate turned 90° and square to its own smallest rectangle. When
  large parts leave gaps, gathering in a corner and then centring is tried and the better kept.
  Objects keep clear of the printer's `bed_exclude_area` and the prime tower with its brim.
- Arrange all plates (A, the toolbar button), the current plate (Shift+A, the plate button),
  the selection round the rest (object menu); locked plates are left alone. What does not fit
  goes on new plates; a new object (add, import, paste, clone, cut pieces) goes beside what is
  there, or on a new plate when its plate is full. Arrange settings (spacing, auto rotate) on
  a right-click of the toolbar button, kept in the preferences.
- Auto orient (`model::orient`, after OrcaSlicer's Tweaker-based method): the six axes and the
  directions most of the surface faces are scored by the area left overhanging past the
  support angle, the area resting on the plate and the height; the model turns only when that
  is clearly better, where it stands.
- Shared instances, as OrcaSlicer's: + adds an instance, − removes one, "Set as an individual
  object" splits one off; instances share mesh, parts, paint, settings, ranges, ears and layer
  heights (an edit to one by any route reaches the others), each placed on its own; saved as
  one 3MF object with several build items, and Orca's multi-instance objects open that way.
- The prime tower drawn on Prepare's plates where it will print (its depth estimated from the
  flushing volumes as the engine sizes it), dragged on the canvas, its place kept per plate in
  `wipe_tower_x`/`wipe_tower_y` as OrcaSlicer does and given to that plate's slice.
- Plates: slices kept per plate; "Slice all" slices every plate in turn; File › Export ›
  "Export G-code of all plates" writes each plate's G-code into a folder; Preview's plate strip
  shows each plate's thumbnail, the slice state, and switches plate. (Dragging objects between
  plates, per-plate name, bed type and sequence used by the slice were already there.)
- Checked by tests: 30 mixed parts (cubes, cylinders, cones, spheres, tori, turned and
  stretched) heaped in the middle are arranged onto one plate inside the printable area with
  no two overlapping; twelve 60 mm cubes fill a plate with nine and put three on a new one;
  arranged objects keep off an excluded area and the tower; a flipped cone stands on its base.

Moved on to session 19: OrcaSlicer's "allow multiple materials on same plate", "align to Y
axis" and "avoid extrusion calibration region" arrange options; nesting into concave
pockets (footprints are convex, as Orca's default); exporting all plates as one sliced 3MF
(`.gcode.3mf`); instances shown as their own rows in the object list (here a label says
"instance k of n"); plate reordering by drag.

## 15 · Printer connections and Device pages — done
- `crates/connect`, with no async runtime: a small HTTP/1.1 client (plain or TLS through
  rustls, Basic / Digest / API-key auth, chunked replies, multipart uploads, every size capped,
  every read and write timed out), a minimal MQTT 3.1.1 client, implicit FTPS, MD5.
- Physical printers (Preferences, by name) with OrcaSlicer's host types. Test connection,
  upload, and upload-and-print for OctoPrint and Klipper through Moonraker (found out by
  asking), PrusaLink (digest login or API key; storage listed and chosen), Duet
  (`rr_upload`, `M32`), Repetier (by printer slug), AstroBox, ESP3D, MKS (upload, then `M23`/
  `M24` on port 8080) and FlashAir (upload only). PrusaConnect, Obico, SimplyPrint,
  CrealityPrint, Flashforge and Elegoo Link say plainly that they are not supported yet.
- Bambu Lab printers in LAN mode, without Bambu's plugin: found by their SSDP announcements;
  status and control over MQTT/TLS (port 8883, access code; reports merged as they arrive);
  the plate packed as Bambu's sliced 3MF (`Metadata/plate_1.gcode`, its MD5, `slice_info`,
  thumbnail) sent over FTPS (port 990, the data connection resuming the TLS session) and
  started with `project_file` (AMS on or off, filament n from slot n unless mapped); the P1/A1
  camera (port 6000). The dialog explains LAN mode / developer mode.
- "Print plate" slices the plate if it needs to, then uploads (and starts) on the chosen
  printer; "Upload only" leaves it on the printer.
- Device page, live for Klipper, OctoPrint, PrusaLink and Bambu: state, file, progress, layer,
  time left, nozzle / bed temperatures with targets to set, pause / resume / stop, jog (off
  while printing), home, extrude / retract, part fan, Bambu's speed levels, and the camera
  (Moonraker's webcam snapshot, OctoPrint's, PrusaLink's, Bambu P1/A1's stream). Multi-device
  lists every printer with its state, progress and pause / stop.
- Each printer's connection is a worker thread (and the camera another); the window only
  picks up events, so a dead or slow printer never blocks it (checked by a test against a
  closed port: the window's calls return at once and the printer shows offline).
- Checked by tests against fake servers: a Moonraker that takes "Print plate" end to end
  (slice, multipart upload with `print=true`), Moonraker / OctoPrint / PrusaLink status
  readings, PrusaLink's `PUT` with `Print-After-Upload`, Digest auth, an MQTT broker
  exchange, Bambu report merging, commands and the sliced 3MF's checksum.

**Not yet checked on real printers:** the "Done when" (your Klipper printer and a Bambu printer
in LAN mode) needs your printers; Bambu's FTPS session reuse and `project_file` fields
are the parts most likely to need adjusting against current firmware.

Moved on to session 19: WebRTC cameras and the X1's RTSP camera; live status for Duet,
Repetier and the other upload-only hosts; PrusaConnect, Obico, SimplyPrint, CrealityPrint,
Flashforge and Elegoo Link; an AMS slot mapping editor; sending one plate to several printers
from Multi-device; tying physical printers to printer presets; a custom HTTPS CA file.

## 16 · Calibration — done
- The Calibration menu as OrcaSlicer's: Temperature; Flow rate (Pass 1, Pass 2, YOLO,
  YOLO perfectionist); Pressure advance (Line, Pattern, Tower); Retraction test; Max
  flowrate; VFA; Input shaping frequency and damping; Cornering. Each opens a dialog with
  Orca's defaults (the temperature range by filament type, junction deviation or jerk by
  firmware), replaces the project with the test, and then asks what was read off the print.
- The tests are set up as Orca's Plater does: Orca's models (packed into `assets/calib/` by
  `tools/pack_calib_models.py`), cut to the range asked for (the temperature tower to its
  blocks, the others to their height), scaled to the nozzle, and the settings each needs as
  edits to the presets or as the object's own settings (the flow blocks each with its
  `print_flow_ratio`, which the engine now honours per object).
- Engine (`calib`): Orca's per-layer stepping (`interpolate_value_across_layers`) for
  temperature, pressure advance, retraction length, outer wall speed (VFA, and max volumetric
  speed converted to mm/s for the test's line), input shaping (Klipper `SET_INPUT_SHAPER`,
  Marlin 2 / RepRapFirmware `M593`) and cornering (`M205 J`, Klipper's square corner
  velocity, jerk); the PA line and pattern are drawn as toolpaths (each line or corner its own
  island with its PA value; Orca's seven-segment numbers), so the Preview, the estimate and the
  start and end G-code are as for any print.
- Results saved into the presets as edits: nozzle temperature, flow ratio (×(100+n)% for the
  passes, +n for YOLO), pressure advance (the tower's from the measured height), retraction
  length, max volumetric speed, input shaper frequency and damping, junction deviation or jerk.
  VFA has nothing to save (its instructions say what to look for).
- Checked by tests: every test sets up, fits the plate, slices, and its G-code carries its
  changes (falling `M104`s 5 °C apart, growing retractions all given back, a PA command per
  line or layer, input shaper and cornering commands); results land in the right presets; the
  temperature tower is cut to nine blocks for 230–190 °C; the flow blocks get Orca's ratios.
- `--calibrate <test>` sets a test up from the command line (screenshots).

Not like Orca yet (moved to session 19): the PA pattern has no frame round it and prints its
numbers on its last layer; Orca's PA test option for bed-mesh accelerations and speeds; the
"Orca tolerance test"; belt-printer variants; the calibration tab's guided flow for Bambu
printers (their automatic calibration needs Bambu's network plugin).

## 17 · App completeness — done
- Translations: OrcaSlicer's catalogues for its 22 languages, packed by
  `tools/pack_translations.py` down to the English texts CosmicSlicer shows (every setting's
  label, tooltip, unit and choice, page and group names, and the UI's own strings where Orca
  has the same wording; about 1,950 per language). Shown through `i18n::tr`: settings, menus,
  window titles, tabs, the sidebar, buttons, tooltips, Home and the shortcut list. Chinese,
  Japanese, Korean and Thai take a font from the system (Noto, Droid, WenQuanYi, Microsoft and
  Apple fonts are looked for); a notice says when none is found. Orca has no right-to-left
  language, so there is nothing to mirror. Language in Preferences (or the system's).
- Preferences as OrcaSlicer's pages that apply, each wired to what it says: language, units,
  default page, dark mode, advanced settings, multi-device; 3MF load behaviour (load all, ask
  when the presets differ, always ask, geometry only), auto backup interval (or off), maximum
  recent files, models in the recent list; arrange after cloning, arrange spacing and
  rotation; remember the print button's choice; camera style (default or touchpad: Alt+move
  orbits, Shift+move pans), orbit speed, zoom to mouse, reverse zoom, perspective, what each
  mouse button's drag does; 3D mouse; the shortcut list; log level and the data folder.
- Keyboard shortcuts: OrcaSlicer's table (`Shortcuts.cpp`) for every feature CosmicSlicer has:
  Ctrl+Shift+G print, Ctrl+Shift+A all plates, Ctrl+D delete all, V printable, Shift+Q orient
  plate, arrows 10 mm (Shift 1 mm), Page Up/Down 45°, E brim ears, Ctrl+0–7 views, I/O zoom,
  Ctrl+E labels, Ctrl+F search, Shift+G jump to layer, Preview's ←/→/Home/End, the painting
  brushes (C/S/F) while painting, F5, and the rest as before. One table drives the handler
  and the Keyboard Shortcuts window; a test checks no two clash.
- 3Dconnexion mice through spacenavd (Linux/BSD) on their own thread: twist and tilt orbit,
  push/pull zooms, slide pans, with a speed setting.
- Project page: model information as before, OrcaSlicer's profile notes (title, description)
  and attachments (Model Pictures shown, Bill of Materials, Assembly Guide, Others) added,
  saved out and removed, all saved in the 3MF (`Auxiliaries/`, `ProfileTitle`,
  `ProfileDescription`) and read back from Orca's.
- Home: online model sites listed (Printables, MakerWorld, Thingiverse, Cults) besides the
  manuals and recent projects.
- Accessibility: icon buttons carry their names for screen readers (AccessKit). Every page and
  dialog is drawn in German, Japanese and Thai at 1×, 1.25×, 1.5× and 2× pixel density,
  changing scale between frames as moving to another monitor does (both themes are drawn by
  the other smoke tests).

Moved on to session 19: re-binding shortcuts (Orca lets them be reassigned); 3Dconnexion on
Windows and macOS (3Dconnexion's driver interface); STEP import tolerances as preferences;
OrcaSlicer's "Speed Dial"; translating CosmicSlicer's own wording (texts Orca does not have stay
English); attachments in autosaves (they live in the project file).

## 18 · Hardening — done (loose ends listed below)
- Fuzz the STL/OBJ/3MF/STEP/G-code readers and the engine with random meshes and settings
  (cargo-fuzz plus property tests); every input either slices or gives an error.
- Performance: parallel per-layer everything, memory caps for huge models, Preview level of
  detail; budgets measured on a reference set and checked in CI.
- Soak test: hours of scripted UI actions (load, edit, slice, undo, switch pages) on all three
  platforms with no panic recovered and no memory growth.
- From session 10: mesh booleans between parts (union, difference, intersection) and repair
  of self-intersections.

Done so far (2026-11-02):
- A mutation fuzzer that runs with the normal tests (`import::fuzz`; cargo-fuzz needs
  nightly): every model reader and the zip/gzip layers, each part of a 3MF project, the
  G-code viewer, the engine with random values for any setting (half from the options it
  reads) and with broken meshes (moved, merged, dropped and flipped triangles, bad indices,
  non-finite coordinates). A case fails on a panic, `SliceError::Bug`, a minute or 512 MB.
- Found and fixed: SAT loops whose damaged pointers never close (4 GB), a record number
  overflow, `brep::orient_outward` gridding sliver triangles (1.5 GB), and CAD surfaces
  meshed evenly at their tightest bend: the trimmer's parameter space is now scaled by the
  step the tolerance allows in each direction (a cylinder 140 000 → 1 430 triangles, a real
  STEP part 10 104 → 3 334 at the same volume). Port these to CosmicDMS, which the import
  crate is vendored from.
- `tools/capped.sh` runs cargo in a memory-capped scope with logs in `target/diag/` (a
  runaway test once took VS Code down).
- Performance: `Progress::timings` gives seconds per stage; `budget_tests` slices a reference
  set (Orca's calibration towers, spheres with tree and grid support, a gyroid torus) against
  time budgets and a 1.5 GB peak, run on its own in CI. Faster: i_overlay in 32-bit integers
  (2× per offset), narrow-part detection that reuses the walls' centre-line offset and stops
  early, support toolpaths and travel regions made in parallel, column supports split into
  parallel growth, a sequential carry and parallel finishing. G-code memory: the motion
  planner takes blocks as they come and the text is written straight after the header (1.19
  → 0.90 GB peak on the speed test structure). Benchy 6.5 → 3.7 s, the KSR FDM test with
  supports and gyroid 38.7 → 14.5 s, the speed test structure 47 → 29 s. The 300 mm gyroid
  takes 0.39 s.
- Soak test (`app::soak`): random use of the whole app through the session's methods with
  frames drawn (add, select, move, scale, mirror, cut, split, copy, delete, undo and redo,
  plates, pages, any setting, arrange, slice); no panic, and memory levelling off once the
  undo history is full. CI runs 400 steps with the run number as the seed; 600 and 1 500
  steps (seeds 1 and 2, 194 slices) ran clean here.
- Mesh booleans (`model::boolean`, OrcaSlicer's Mesh boolean tool in the object menu): union,
  difference and intersection of the selected objects into one new object in their place.
  Exact orientation tests (`robust`) decide where an edge of one mesh passes through a
  triangle of the other; each such point is made once, so neighbouring triangles share it and
  the result is closed without welding. Cut triangles are retriangulated in their own
  parameters (points on an edge exactly on it) and patches between the cuts are kept or
  dropped by their winding number. The second mesh is shifted by 1e-7 of the size so nothing
  coincides exactly. Fuzzed on 2 000 random pairs of primitives (half on a grid, with shared
  faces, edges and corners): always closed, and union + intersection = both, difference =
  first − intersection. Benchy with a cylinder through it: 0.2 s, closed.
- "Fix model" in the object menu: repair plus crossing shells merged into one solid (cavities,
  facing inwards, kept). Self-intersections within one shell are not repaired yet.
- The round primitives (sphere, cone, torus) were open at their poles, tip and seam; `lathe`
  now shares those vertices, and every primitive is checked closed.
- Still open: offsets cost ~1 ms each in i_overlay (the speed test structure's walls);
  self-intersections within a shell; booleans run on the UI thread (Benchy 0.2 s).

## 19 · Parity with OrcaSlicer — in progress
Progress (2026-11-02):
- `tools/parity.sh` slices a reference set (eight generated shapes and Orca's own Benchy, Voron
  cube, KSR test, Orca cube, calicat and Stanford bunny) with OrcaSlicer 2.4.2's command line on
  the X1C, MK4 and Voron 2.4 presets, and with ours on the settings Orca wrote into its G-code;
  `parity_tests` compares time, filament, features, retractions and toolpath maps, and draws
  layers. Every difference so far was fixed from Orca's own source.
- Fixed: estimates (custom G-code and waits timed; "model printing time" as Orca's), layer
  cooling (Orca's cooling buffer and fan rules), auto brim, retraction and wipe, vertical shells
  and internal bridges (which areas, which direction, spacing), infill alignment, spacing and
  joining, Orca's crosshatch, narrow solid infill, monotonic lines, narrow outer loops (pin tips),
  and a slicing bug that lost whole layers when a vertex lay on the cutting plane.
- Now: time within about -6 % to +8 % of Orca's estimate across 42 cases, filament within about
  2 %, toolpaths covering the same ground 60–94 %.
- Still open: "slowdown for curled perimeters" (Orca's curl estimate; the MK4 overhang test runs
  9 % faster than Orca's); the MK4 (Arachne) infill area is a little wider than Orca's;
  the start sequence's heating and loading time Orca adds to its total.

- A reference set (calibration cube, Benchy, thin walls, overhang test, multi-part, multi-colour,
  large and small parts) sliced by both with the same profiles.
- Compare time, filament, layer count, per-feature totals and toolpath maps; explain or fix every
  difference over a few percent; keep the set as a regression test.
- Test prints on your printers.
- From session 6: Arachne's wall-count transitions where a part narrows but still has room for
  walls; `make_overhang_printable_hole_size`; `scarf_overhang_threshold`.
- From session 10: wall settings inside modifiers and parts (Orca's per-region perimeters).
- From session 11: painting on volumes other than the main mesh, brushes that split triangles
  finer than the mesh (Orca's triangle selector), moving volumes with the canvas gizmos.
- From session 12: flush into infill / objects / support; wall and infill filaments (re-extract
  Orca's settings to get `wall_filament`, `sparse_infill_filament`, `solid_infill_filament`).
- From session 13: per-object layer stacks; independent support layer height; spiral vase
  XY smoothing; leading-edge-only brims; the layer profile drawn beside the object.
- From session 17: re-binding shortcuts; 3Dconnexion on Windows and macOS; STEP tolerance
  preferences; the Speed Dial; translations of CosmicSlicer's own texts.
- From session 16: the PA pattern's frame and Orca's numbering layout; the PA test's
  acceleration/speed lists; the tolerance test.
- From session 15: WebRTC and X1 (RTSP) cameras; live status for Duet, Repetier and the other
  upload-only hosts; PrusaConnect, Obico, SimplyPrint, CrealityPrint, Flashforge, Elegoo Link;
  AMS mapping editor; one plate to several printers; physical printers tied to presets;
  HTTPS CA file. Prints on a real Klipper and a real Bambu printer in LAN mode.
- From session 14: arrange options for multiple materials per plate, Y-axis alignment and the
  calibration region; concave nesting; all plates as one `.gcode.3mf`; instances as their own
  rows in the object list; reordering plates.

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

# G-code parity with OrcaSlicer 2.4.2

The goal: the same model and settings give the same G-code as OrcaSlicer. Where they differ, this
file says why. Numbers come from `tools/parity.sh` then `tools/parity_diff.py --all <dir>`, over
14 models. The reference printer is now the Bambu Lab X2D (`tools/parity.sh <dir>`, numbers below);
the X1C, MK4 and Voron table further down was taken before this stretch's work (2026-11-05).

"Identical lines" counts text lines, so one different seam or one extra progress line counts.
"Same place" counts millimetres of extrusion within 0.01 mm of an Orca segment with the same ends.

## Where it stands on the X2D

0.20mm Standard @BBL X2D, Bambu PLA Basic @BBL X2D 0.4 nozzle, single filament (2026-11-05):

| Model | Identical lines |
|---|---|
| cube20 | 94 % |
| cylinder | 91 % |
| thinwalls | 86 % |
| small | 75 % |
| overhangs | 72 % |
| bridge | 70 % |
| calicat | 68 % |
| steps | 67 % |
| ksr_fdmtest_v4 | 62 % |
| large | 57 % |
| 3DBenchy | 50 % |
| OrcaCube_v2 | 46 % |
| Voron_Design_Cube_v7 | 47 % |
| Stanford_Bunny | 7 % |

What the X2D does that the X1C does not: two nozzles put it on Orca's "traditional timelapse" path
(the retract, lift, end of object label and picture come at the end of a layer, or before leaving
the pictured nozzle, not after the next layer's header; `timelapse::Picker` is
`TimelapsePosPicker`, printing by layer only so far), and its filament overrides the printer's
retraction, lift and wipe (0.4 mm against 0.8 mm).

What is left on the X2D, largest first:

- Seams (Benchy's outer walls 50 %, see below); a seam that lands elsewhere also moves the wipe, the
  lift, and the travel to whatever comes next.
- Concentric internal fill: about 40 % of Benchy's solid length, and the corners of `large`. Orca
  fills narrow solid with `FillConcentricInternal`, which is Arachne; ours is a plain offset. Those
  pieces also leave the rest of the region a different shape, so its bounding box moves every
  rectilinear line by under a micron. Needs `split_narrow_solid` in integers and Arachne.
- Solid regions: where lines differ on Benchy layers 31 and 41, Orca's region has a small protrusion
  ours lacks, which shifts all the lines. The lines themselves match wherever the region does.
- Layer cooling: a differing layer time gives a differing `F` on that layer's slowed moves.
- `M73`: Orca writes them when the percentage or minute changes, ours at each layer, and our
  estimate leaves out the start G-code.
- `M106`: Orca defers a fan change to just after a move (`need_set_fan`); ours comes before it.
- The first layer's order of `M204`, `M106 S0` and the first lift.
- Two colours on the X2D: the prime tower's area in the picker, and picking the most used nozzle
  (the first filament's, for now). Printing by object gets no safe picture position.
- Bridges and brim, as on the X1C.

## On the other printers, before this stretch

Identical lines by model:

| Model | X1C | MK4 | Voron |
|---|---|---|---|
| cube20 | 89 % | 79 % | 73 % |
| cylinder | 90 % | 45 % | 42 % |
| thinwalls | 78 % | 43 % | 44 % |
| small | 75 % | 65 % | 63 % |
| ksr_fdmtest_v4 | 66 % | 31 % | 32 % |
| bridge | 66 % | 56 % | 59 % |
| calicat | 64 % | 48 % | 52 % |
| steps | 62 % | 48 % | 46 % |
| large | 59 % | 54 % | 39 % |
| 3DBenchy | 46 % | 11 % | 11 % |
| OrcaCube_v2 | 44 % | 13 % | 14 % |
| Voron_Design_Cube_v7 | 47 % | 15 % | 19 % |
| overhangs | 40 % | 27 % | 31 % |
| Stanford_Bunny | 14 % | 4 % | 5 % |

MK4 and Voron use Arachne walls, which are not ported yet; the X1C uses classic walls, which are.
Without arc fitting the X1C cylinder reaches 94 %.

Benchy on the X1C, millimetres in the same place: gap infill 95 %, inner wall 92 %, sparse infill
92 %, top surface 91 %, internal bridge 53 %, outer wall 50 %, bottom surface 40 %, internal solid
33 %, bridge 0 %.

## The lesson of these numbers

Orca's "ties" are mostly not ties. Two infill arcs that should be equal differ by a nanometre, and
a cylinder's vertex angles differ by 1e-7 radians, so which one wins depends on exact integers from
the mesh onwards. Every mm-and-double approximation upstream reshuffles those choices downstream:
porting the slicer's rounding alone took Benchy's inner walls from 31 % to 92 %. So port arithmetic,
not just algorithms, and never imitate a tie-break: both imitations this project tried (sorting
lines by row, starting seam polygons at the rear) matched one test model and broke others.

## Ported from Orca's source, so these match by construction

- `rectilinear`: `fill_surface_by_lines`, for rectilinear, monotonic and monotonic line object fills:
  the rotated region, exact rational cuts, contour links, monotonic regions and the ant colony with
  its seed. Orca's `link_max_length` is `3 * f->spacing` taken before `f->spacing` is set, so it is
  always 0 and links are unlimited.
- `chain`: `chain_segments_greedy_constrained_reversals` with `KDTreeIndirect` and
  `MutablePriorityQueue`. `Region::append` lets a sortable fill's paths loose, so all of a region's
  fills chain together; only fills that keep their order stay one piece. Grid lines never reverse
  (`can_reverse`, set in `Fill.cpp`): `FillGrid` turns them round on odd layers instead.
- `rectilinear::crossing_lines` with `infill::crossing`: `fill_surface_by_multilines` and
  `make_fill_lines`, for grid, triangles, tri-hexagon, cubic, quarter cubic and lateral lattice.
  Checked against Orca on the cube: 100 % of each pattern's sparse lines, same volume.
- `config::variants`: `update_values_to_printer_extruders`, one value per nozzle from the variants a
  profile lists.

Mesh slicing (`slicing.rs`: float vertices, `slice_facet`, closing, simplification), Clipper and its
wrappers (`clipper.rs`, `geom.rs`), classic walls and gap fill in Orca's integers (`walls.rs`),
simplifying and arc fitting before seams (`toolpath::simplify`), seam placement (`seam.rs`), infill
line joining (`connect.rs`), cross hatch (`infill.rs`), arc fitting (`arcs.rs`), number and move
format, layer change order, lifts, wipes, object labels, line widths, narrow solid detection,
monotonic lines, line ends, file header, footer, start and end order, machine limits.

## The app's presets and Orca's differ

These numbers take their settings from Orca's G-code. `PARITY_LIBRARY=1` slices with our own library
instead, as the app does. Our pack came from newer Orca source (Bambu profiles 02.08) than Orca 2.4.2
ships (02.01), so on the X2D the app differs from an Orca 2.4.2 slice by the profile itself: `grid` not
`crosshatch`, newer start, layer change and timelapse G-code, and a few process and filament values.
On the cube that is 54 % of lines identical. Each difference is a profile value, not the slicer.

## Differences that cannot go away

| Difference | Why |
|---|---|
| `; generated by` line | Names the program. |
| Date and time in that line | Orca stamps the clock; ours does not. |
| Object label ids (`id:104045582745603`) | Orca's internal numbers, different on every run of Orca itself. Two runs of Orca differ in these lines and the timestamp only. |
| Thumbnails | Orca's command line has no renderer, so it writes none. The app does. |

## Differences with a known cause, fixable with more porting

| Difference | Cause | What it would take |
|---|---|---|
| MK4 and Voron walls, 30 to 34 % | Their profiles use Arachne. Ours uses the medial axis for the same job, so widths and vertices differ. | Port Arachne, a large job. |
| Arcs on curved walls | Orca fits arcs on the loop as generated, then splits at the seam. We fit after the seam, starting elsewhere, so breaks fall on other vertices. | Fit before placing the seam and carry arcs through `Path::rotate`. |
| M73 progress lines | Orca places them from its own time estimate after slicing. Ours sit at layer starts. Every layer has some. | Port the post-processor's placement. |
| Cooled speed, a few units off (F4641 vs F4644 on the cube) | Decided by total layer time, so it settles only when the toolpath does. Includes Orca's parser quirk, ported. | Follows from the rows above. |
| Time and filament totals | Orca's estimator also counts heating and loading; `filament used` includes the start purge. | Port `GCodeProcessor` accounting. |

## Seams still to pin down

Measured on outer wall loops, exact and within 1 mm of Orca's start: cube20, cylinder, large and small
100 %, thinwalls 97 and 97, overhangs 83 and 100, calicat 80 and 84, ksr 79 and 86, Benchy 67 and 89,
Voron cube 38 and 86, Bunny 46 and 80, steps 50 and 50, OrcaCube 9 and 36, bridge 2 and 2.

The steps and bridge misses are shapes symmetrical enough that only visibility noise tells corners
apart: Orca takes the rear inner corners of bridge's pillars and we take the front ones. Read line for
line against Orca and found equal: the sampling, the ray frame, the candidate heights, the seam string
and the fit. The samples themselves look right: changing the order of the random draws or of the
triangles takes the Voron cube from 86 % to under 80 %, and arrange's quarter turns do not explain
steps. What is left cannot be read from Orca's G-code; it needs a build of Orca that prints its scores.

## Not traced yet

These differ and the cause is not established.

- Bridges 0 %, gap infill 0 %, prime tower 0 %.
- Benchy internal solid and top surfaces under 1 %, where the cube reaches 96 and 100 %.
- Narrow solid corners: Orca fills them with Arachne at variable width.
- `calicat` and the Bunny come out one or two layers short of Orca.
- Multi-colour flushing: Orca purges 64 to 82 mm per change, ours 45.6.
- Config block: values are written unquoted, and vector options can have fewer entries than Orca's.
- Chaining of separate infill pieces: we take the nearest next, Orca uses its own chain search.

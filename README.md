# CosmicSlicer

A slicer for FFF 3D printers, written in Rust: a rebuild of OrcaSlicer with a focus on never
crashing. Runs on Windows, macOS and Linux (egui + wgpu: DirectX 12, Metal, Vulkan or GL).

## Status
Step 1 — the user interface — is done: OrcaSlicer's Home, Prepare, Preview, Device, Multi-device
and Project pages; the sidebar (printer, filaments, objects, process); every OrcaSlicer setting
on its page and group, with Advanced, search, per-object settings, presets, saving and comparing;
the 3D plates with picking, Move / Rotate / Scale / Lay on face, arrange, auto orient, split,
multiple plates, undo / redo; STL and OBJ import, STL export.

Step 2 — the slicing engine — is done: layers, walls (inner/outer order, aligned / nearest /
back / random seams), top / bottom / bridge / solid / sparse regions, infill (rectilinear,
monotonic, grid, triangles, cubic, gyroid, concentric; other OrcaSlicer patterns map to the
nearest), skirt, brim (incl. auto), normal supports with interfaces, ironing, retraction and
z-hop, layer cooling and fan, the volumetric speed limit, tool changes, custom G-code with
placeholders, and estimates of time, filament, weight and cost. Slicing runs in the background
with progress and Cancel; Preview draws the real toolpaths with OrcaSlicer's legend and view
types; Export G-code writes the file.

Not yet: tree supports (printed as normal supports), rafts, prime tower, gap fill, arc fitting,
variable layer height, painting tools, 3MF projects and printer connections.

Script it: `CosmicSlicer model.stl --set sparse_infill_density=30% --export out.gcode`.

## Build and run
```
cargo run --release
cargo run -- --help          # command-line options, including screenshot runs
cargo test --workspace
```
Linux needs the usual X11/Wayland development packages for winit.

## License
AGPL-3.0-or-later, as OrcaSlicer. See `ATTRIBUTION.md`.
# CosmicSlicer

"""Packs OrcaSlicer's calibration models into assets/calib/.

Orca ships them Draco-compressed (`.drc`); CosmicSlicer has no Draco reader, so each is decoded
here (with DracoPy) and written as a zlib-compressed binary STL (`name.stl.z`). The flow-rate
tests are Orca 3MF projects and are copied as they are.

Usage: python3 tools/pack_calib_models.py <OrcaSlicer>/resources/calib assets/calib
(needs `pip install DracoPy numpy`)
"""
import os, shutil, struct, sys, zlib

import DracoPy
import numpy as np

src, out = sys.argv[1], sys.argv[2]
os.makedirs(out, exist_ok=True)
MODELS = [
    "temperature_tower/temperature_tower.drc",
    "pressure_advance/tower_with_seam.drc",
    "retraction/retraction_tower.drc",
    "volumetric_speed/SpeedTestStructure.drc",
    "vfa/vfa.drc",
    "input_shaping/ringing_tower.drc",
    "input_shaping/fast_tower_test.drc",
    "cornering/SCV-V2.drc",
]
FLOW = [
    "filament_flow/flowrate-test-pass1.3mf",
    "filament_flow/flowrate-test-pass2.3mf",
    "filament_flow/Orca-LinearFlow.3mf",
    "filament_flow/Orca-LinearFlow_fine.3mf",
]
for rel in MODELS:
    m = DracoPy.decode(open(os.path.join(src, rel), "rb").read())
    pts = np.asarray(m.points, dtype=np.float32).reshape(-1, 3)
    faces = np.asarray(m.faces).reshape(-1, 3)
    buf = bytearray(80) + struct.pack("<I", len(faces))
    for a, b, c in faces:
        buf += struct.pack("<12fH", 0, 0, 0, *pts[a], *pts[b], *pts[c], 0)
    name = os.path.basename(rel)[:-4] + ".stl.z"
    open(os.path.join(out, name), "wb").write(zlib.compress(bytes(buf), 9))
    print(name, len(faces), "triangles")
for rel in FLOW:
    shutil.copy(os.path.join(src, rel), os.path.join(out, os.path.basename(rel)))
    print(os.path.basename(rel))

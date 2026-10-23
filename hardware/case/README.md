# Case

A two-part case for the Pi Zero W and the Waveshare 2.13" e-Paper HAT. It needs no supports.

| File | What it is |
|---|---|
| `case_plate.stl` | Both parts laid out for printing together |
| `case_base.stl` | The base, printed floor down |
| `case_lid.stl` | The lid, printed face down (it's already flipped) |
| `case.scad` | The parametric OpenSCAD source |
| `check_overhangs.py` | Checks that every STL prints without support |
| `Makefile` | Re-exports the STLs and runs the check |

- **Size:** 71.8 × 36.9 × 22.3 mm outside.
- **Openings:**
  - One 24.6 × 8 mm slot that fits plugs in both micro-USB ports (data and power).
  - A notch for the SD card, which sticks out past the board edge.
  - The mini-HDMI cut-out is off by default; tick `HDMI_NOTCH` in the Customizer to add it.
- **Fastening:**
  - The Pi screws to the base standoffs with four countersunk M2.5 × 6 screws, driven up through the floor. They self-tap into a 2.2 mm hole.
  - Optionally, put M2.5 × 11 spacers between the Pi and the HAT.
  - The lid snaps onto the base. A 45° ridge on each long side of the 3 mm lip clicks into a groove inside the lid. To open it, lever up at the USB slot with a fingernail.

## Print settings

- PLA or PETG, 0.2 mm layers, 3 perimeters, no supports, no brim needed.
- Put the lid face down so that the face of the case is the smooth side that was on the bed.
- `make` runs `check_overhangs.py`, which reports any surface steeper than 45° that isn't on the bed, including bridges. It should report 0 for every part.

## Measure first

The design follows the official Pi Zero drawing. These values vary between units, so check yours with calipers before a full print. All of these appear in the Customizer:

- **`HAT_GAP` (11 mm):** the distance from the top of the Pi PCB to the underside of the HAT. It depends on the header you solder on.
- **`WINDOW_OFFSET`:** the window assumes the panel's active area is centred on the board. If it isn't, measure the offset and enter it here.
- **`FIT` (0.15 mm):** the gap between the lid and the lip. Increase it if the lid is too tight, decrease it if the lid is loose.
- **`SNAP_D` (0.4 mm):** how far the snap ridge stands out from the lip. The lid has to flex past `SNAP_D − FIT` (0.25 mm) to close. Raise `SNAP_D` for a firmer click, lower it if closing the lid is a struggle.

## Edit and regenerate

Open `case.scad` in OpenSCAD. The Customizer panel (Window → Customizer) shows the settings above, plus a `part` selector (`assembly`, `base`, `lid` or `plate`) and an `explode` slider for the assembly view.

To re-export the STLs:

```sh
make -C hardware/case    # writes the three STLs, then checks them for overhangs
```

// Two-part, support-free case for a Pi Zero W + Waveshare 2.13" e-Paper HAT.
//
// Why it is shaped like this:
// * Every opening for a USB plug or the SD card is a notch cut *down from the
//   top of the base wall*. The lid's skirt closes the top of each notch, so the
//   base never needs a bridge or support, yet the hole is a closed slot once
//   assembled.
// * The lid prints face-down: the window is a plain through-hole in the first
//   layers, and the window bevel and outer edge chamfer are both 45 degrees.
// * The base prints floor-down. Screw holes are countersunk (a 45 degree cone),
//   not counterbored, because a counterbore's ceiling would be a bridge.
// * The lid is a friction fit over a lip on the base; tune FIT for your printer.
//
// Coordinates follow the Raspberry Pi Zero mechanical drawing: the board's
// lower-left corner is the origin, the SD card is at x = 0, and the mini-HDMI
// and micro-USB ports are on the y = 0 edge. z = 0 is under the base floor.
//
// Export: see README.md, or `make -C hardware/case`.

/* [Part] */
part = "assembly"; // [assembly, base, lid, plate]
// Lift the lid this far in the assembly view
explode = 6; // [0:1:30]

/* [Board and HAT: measure yours] */
// Pi top to HAT underside: 2.5 mm male header plastic + 8.5 mm female header
HAT_GAP = 11.0;
// Active-area centre relative to board centre
WINDOW_OFFSET = [0, 0];
// Extra opening around the active area on each side
WINDOW_MARGIN = 1.2;

/* [Fit] */
// Gap between lip and lid skirt
FIT = 0.15;
// Lid face sits this far above the glass
GLASS_GAP = 0.3;
// How far the snap ridge stands proud of the lip; the click is SNAP_D - FIT
SNAP_D = 0.4;

/* [Openings] */
// The card sticks ~2.5 mm past the board edge, so leave this on
SD_NOTCH = true;
HDMI_NOTCH = false;

/* [Hidden] */
$fn = 64;
e = 0.01;

PI_W = 65.0; PI_D = 30.5; PCB_T = 1.6;
HOLES = [[3.5, 3.5], [61.5, 3.5], [3.5, 26.5], [61.5, 26.5]];
USB_OTG_X = 41.4; USB_PWR_X = 54.0; HDMI_X = 12.4;
SD_Y = 16.9;
HAT_T = 1.6;
GLASS_T = 1.2;                         // e-paper glass + tape on the HAT PCB
ACTIVE_W = 48.55; ACTIVE_D = 23.71;
WINDOW_BEVEL = 0.8;

WALL = 2.4;
FLOOR = 2.0;
TOP = 1.6;
CLEAR_X = 1.0;                         // each short end (display FPC fold)
CLEAR_PORT = 1.0;                      // connectors sit at or just past the edge
CLEAR_BACK = 0.6;
STANDOFF_H = 3.0; STANDOFF_D = 5.5;    // clears the header solder joints
SCREW_D = 2.2;                         // M2.5 self-tapping into plastic
CSK_D = 5.0;                           // M2.5 flat-head countersink
LIP_T = 1.0; LIP_H = 3.0;
R_OUT = 3.0;
EDGE_CHAMFER = 0.8;

PLUG_W = 12.0; PLUG_H = 8.0;           // micro-USB overmould ~11 x 7 mm
HDMI_W = 16.0;

PI_BOT = FLOOR + STANDOFF_H;
PI_TOP = PI_BOT + PCB_T;
PORT_Z = PI_TOP + 1.5;                 // micro-USB centre line
NOTCH_BOT = PORT_Z - PLUG_H / 2;
SPLIT = PORT_Z + PLUG_H / 2;           // top of base wall = top of the USB slot
GLASS_TOP = PI_TOP + HAT_GAP + HAT_T + GLASS_T;
HEIGHT = GLASS_TOP + GLASS_GAP + TOP;

IN0 = [-CLEAR_X, -CLEAR_PORT];
IN1 = [PI_W + CLEAR_X, PI_D + CLEAR_BACK];
OUT0 = IN0 - [WALL, WALL];
OUT1 = IN1 + [WALL, WALL];

module rrect(p0, p1, r) {
    r = max(min(r, (p1.x - p0.x) / 2 - e, (p1.y - p0.y) / 2 - e), e);
    translate(p0 + [r, r]) offset(r) square(p1 - p0 - [2 * r, 2 * r]);
}

module outline(grow = 0) rrect(OUT0 - [grow, grow], OUT1 + [grow, grow], R_OUT + grow);
module cavity(grow = 0) rrect(IN0 - [grow, grow], IN1 + [grow, grow], R_OUT - WALL + grow);

module prism(z0, z1) translate([0, 0, z0]) linear_extrude(z1 - z0) children();

module box(p0, p1) translate(p0) cube(p1 - p0);

// The outer solid with 45 degree chamfers on its bottom and/or top edges.
module chamfered_shell(z0, z1, bottom, top) {
    c = EDGE_CHAMFER;
    hull() {
        prism(z0 + (bottom ? c : 0), z1 - (top ? c : 0)) outline();
        if (bottom) prism(z0, z0 + e) outline(-c);
        if (top) prism(z1 - e, z1) outline(-c);
    }
}

// Notches cut down from the top of the base wall (open-topped: no bridges).
module openings() {
    top = SPLIT + LIP_H + 1;           // also cuts through the lip
    box([USB_OTG_X - PLUG_W / 2, OUT0.y - 1, NOTCH_BOT],
        [USB_PWR_X + PLUG_W / 2, IN0.y + 0.5, top]);
    if (HDMI_NOTCH)
        box([HDMI_X - HDMI_W / 2, OUT0.y - 1, NOTCH_BOT],
            [HDMI_X + HDMI_W / 2, IN0.y + 0.5, top]);
    if (SD_NOTCH)
        box([OUT0.x - 1, SD_Y - 6.5, PI_BOT - 0.5], [IN0.x + 0.5, SD_Y + 6.5, top]);
}

// Snap fit: a 45 degree ridge along the outside of the lip on each long side
// clicks into a matching V-groove inside the lid skirt. Every face is 45
// degrees or steeper, so neither part needs support in its print orientation.
SNAP_Z = SPLIT + LIP_H / 2;
SNAP_END = 5;                          // keep clear of the lip's rounded corners

// Ridge profile (grow = 0), or the groove that receives it (grow = FIT).
// dir is +1 on the back wall and -1 on the front wall.
module snap_profile(x0, x1, y, dir, grow = 0)
    translate([x0, y, SNAP_Z]) rotate([90, 0, 90]) linear_extrude(x1 - x0)
        offset(delta = grow) polygon([[-dir * 0.3, -SNAP_D], [0, -SNAP_D], [dir * SNAP_D, 0], [0, SNAP_D], [-dir * 0.3, SNAP_D]]);

module snaps(grow = 0) {
    front_y = IN0.y - LIP_T;
    back_y = IN1.y + LIP_T;
    snap_profile(IN0.x + SNAP_END, USB_OTG_X - PLUG_W / 2 - 2, front_y, -1, grow);
    snap_profile(IN0.x + SNAP_END, IN1.x - SNAP_END, back_y, 1, grow);
}

module base() {
    difference() {
        union() {
            difference() {
                union() {
                    chamfered_shell(0, SPLIT, true, false);
                    prism(SPLIT - e, SPLIT + LIP_H) cavity(LIP_T);
                    snaps();
                }
                prism(FLOOR, SPLIT + LIP_H + 1) cavity();
            }
            for (h = HOLES) translate([h.x, h.y, FLOOR - e])
                cylinder(h = STANDOFF_H + e, d = STANDOFF_D);
        }
        for (h = HOLES) translate([h.x, h.y, 0]) {
            translate([0, 0, -1]) cylinder(h = PI_BOT + 2, d = SCREW_D);
            // 45 degree countersink, widest on the bed: prints without support.
            translate([0, 0, -e]) cylinder(h = (CSK_D - SCREW_D) / 2 + e, d1 = CSK_D, d2 = SCREW_D - e);
        }
        openings();
    }
}

// The lid in its assembled position (open side down).
module lid() {
    w = ACTIVE_W + 2 * WINDOW_MARGIN;
    d = ACTIVE_D + 2 * WINDOW_MARGIN;
    module win(grow = 0) rrect([-w / 2, -d / 2] - [grow, grow], [w / 2, d / 2] + [grow, grow], 1.0 + grow);

    difference() {
        chamfered_shell(SPLIT, HEIGHT, false, true);
        prism(SPLIT - 1, HEIGHT - TOP) cavity();
        prism(SPLIT - 1, SPLIT + LIP_H + FIT) cavity(LIP_T + FIT);   // skirt over the lip
        snaps(FIT);
        translate([PI_W / 2 + WINDOW_OFFSET.x, PI_D / 2 + WINDOW_OFFSET.y, 0]) {
            prism(HEIGHT - TOP - 1, HEIGHT + 1) win();
            // Hull of true offsets keeps the bevel at exactly 45 degrees, corners included.
            hull() {
                prism(HEIGHT - WINDOW_BEVEL, HEIGHT - WINDOW_BEVEL + e) win();
                prism(HEIGHT, HEIGHT + e) win(WINDOW_BEVEL);
            }
        }
    }
}

// Both parts in print orientation, with their bounding boxes at the origin.
module base_print() translate(-[OUT0.x, OUT0.y, 0]) base();
module lid_print() translate([-OUT0.x, OUT1.y, HEIGHT]) rotate([180, 0, 0]) lid();

if (part == "base") base_print();
else if (part == "lid") lid_print();
else if (part == "plate") { base_print(); translate([OUT1.x - OUT0.x + 8, 0, 0]) lid_print(); }
else { base(); translate([0, 0, explode]) lid(); }

echo(str("outer ", OUT1.x - OUT0.x, " x ", OUT1.y - OUT0.y, " x ", HEIGHT, " mm, split at z=", SPLIT));

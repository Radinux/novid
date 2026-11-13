"""IR preview widget.

* Enrollment mode: an oval, face-shaped, mirrored preview that follows the
  face. A 3D wireframe (meridians + parallels) is drawn over the face and
  rotates live with the head pose; the oval's contour fills towards the
  requested direction and turns green when that pose is captured.
* Full mode (test page): the whole frame with the detected box and the five
  landmarks.
"""

import math

from gi.repository import Adw, Gdk, GLib, Graphene, Gsk, Gtk

SUCCESS = Gdk.RGBA()
SUCCESS.parse("#2ec27e")

# Contour sectors per step: (centre angle, half-width) in degrees, 0 = right,
# clockwise (cairo). The preview is mirrored: the user's left is screen-left.
# (centre, inner half-width, outer half-width): "more" steps extend the
# first step's sector outwards instead of covering it.
SECTORS = {
    "turn-left": (180, 0, 40),
    "turn-right": (0, 0, 40),
    "look-up": (270, 0, 40),
    "look-down": (90, 0, 40),
}


def _sector_arcs(code, frac=1.0):
    """Arcs (a0, a1) covering `frac` of a sector, growing outwards."""
    centre, inner, outer = SECTORS[code]
    reach = inner + (outer - inner) * frac
    if inner == 0:
        return [(centre - reach, centre + reach)]
    return [(centre - reach, centre - inner), (centre + inner, centre + reach)]
MERIDIANS = (-60, -30, 0, 30, 60)
PARALLELS = (-35, 0, 35)


def _lerp(a, b, k):
    return a + (b - a) * k


def _ellipse_arc(cr, cx, cy, rx, ry, a0, a1):
    cr.save()
    cr.translate(cx, cy)
    cr.scale(rx, ry)
    cr.arc(0, 0, 1, math.radians(a0), math.radians(a1))
    cr.restore()


def _project(lat, lon, yaw, pitch):
    """Point on a unit sphere rotated by the head pose -> (x, y, depth)."""
    la, lo = math.radians(lat), math.radians(lon)
    x, y, z = math.cos(la) * math.sin(lo), math.sin(la), math.cos(la) * math.cos(lo)
    cy_, sy = math.cos(yaw), math.sin(yaw)
    x, z = x * cy_ + z * sy, -x * sy + z * cy_
    cp, sp = math.cos(pitch), math.sin(pitch)
    y, z = y * cp + z * sp, -y * sp + z * cp
    return x, y, z


class CameraView(Gtk.Widget):
    __gtype_name__ = "FacelockCameraView"

    def __init__(self, ring=True, full=False):
        super().__init__()
        self.ring = ring
        self.full = full
        self.face = []
        self.texture = None
        self.tex_size = (1, 1)
        self.state = {}  # step code -> "done" | "skipped"
        self.calibration = None  # 0..1 while the reference pose is measured
        self.front_done = False
        self.active = None  # (code, axis, target) of the current step
        self.value = 0.0  # progress along the active step's axis
        self.on_target = False
        self.pose = None  # smoothed (yaw, pitch) in degrees, relative
        self.pose_target = None
        self.view = None
        self.target = None
        self.set_size_request(260, 300)
        self.set_hexpand(True)
        self.set_vexpand(True)
        self.add_tick_callback(self._on_tick)

    def reset(self):
        self.state.clear()
        self.calibration = None
        self.front_done = False
        self.active = None
        self.pose = self.pose_target = None
        self.queue_draw()

    def set_frame(self, w, h, pixels, face):
        self.texture = Gdk.MemoryTexture.new(w, h, Gdk.MemoryFormat.G8, pixels, w)
        self.tex_size = (w, h)
        self.face = list(face)
        if len(face) >= 4:
            x, y, fw, fh = face[:4]
            self.target = (x + fw / 2, y + fh / 2, fh)
        else:
            self.pose_target = None
            if self.target is None:
                self.target = (w / 2, h / 2, h / 2)

    def set_pose(self, yaw, pitch):
        self.pose_target = (yaw, pitch)

    def _on_tick(self, _widget, _clock):
        if self.target:
            self.view = self.target if self.view is None else tuple(
                _lerp(v, t, 0.2) for v, t in zip(self.view, self.target)
            )
        if self.pose_target:
            self.pose = self.pose_target if self.pose is None else tuple(
                _lerp(v, t, 0.3) for v, t in zip(self.pose, self.pose_target)
            )
        self.queue_draw()
        return GLib.SOURCE_CONTINUE

    def do_snapshot(self, snapshot):
        if self.full:
            self._snapshot_full(snapshot)
        else:
            self._snapshot_oval(snapshot)

    # -- enrollment: oval + wireframe + contour -----------------------------

    def _snapshot_oval(self, snapshot):
        w, h = self.get_width(), self.get_height()
        cx, cy = w / 2, h / 2
        margin = 16
        ry = min(h / 2 - margin, (w / 2 - margin) * 1.3)
        rx = ry / 1.3
        irx, iry = rx - 10, ry - 10  # image sits inside the contour

        rect = Graphene.Rect().init(cx - irx, cy - iry, 2 * irx, 2 * iry)
        clip = Gsk.RoundedRect()
        corner = Graphene.Size().init(irx, iry)
        clip.init(rect, corner, corner, corner, corner)
        snapshot.push_rounded_clip(clip)
        snapshot.append_color(Gdk.RGBA(red=0.07, green=0.07, blue=0.08, alpha=1), rect)
        if self.texture and self.view:
            tw, th = self.tex_size
            fx, fy, fh = self.view
            # Face box fills ~62% of the oval height.
            scale = max(2 * iry * 0.62 / max(fh, 1), 2 * iry / th)
            snapshot.save()
            p = Graphene.Point()
            snapshot.translate(p.init(cx, cy))
            snapshot.scale(-scale, scale)  # mirror like a selfie
            snapshot.translate(p.init(-fx, -fy))
            snapshot.append_scaled_texture(
                self.texture, Gsk.ScalingFilter.LINEAR, Graphene.Rect().init(0, 0, tw, th)
            )
            snapshot.restore()
        # Soft vignette so the wireframe reads on bright faces.
        try:
            snapshot.append_radial_gradient(
                rect, Graphene.Point().init(cx, cy), irx, iry, 0.55, 1.0,
                [_stop(0.0, 0, 0, 0, 0.0), _stop(1.0, 0, 0, 0, 0.55)],
            )
        except (TypeError, AttributeError):
            pass  # purely cosmetic
        if self.pose and self.ring:
            self._draw_mesh(snapshot, w, h, cx, cy, irx, iry)
        snapshot.pop()

        if self.ring:
            self._draw_contour(snapshot, w, h, cx, cy, rx, ry)

    def _draw_mesh(self, snapshot, w, h, cx, cy, rx, ry):
        cr = snapshot.append_cairo(Graphene.Rect().init(0, 0, w, h))
        accent = Adw.StyleManager.get_default().get_accent_color_rgba()
        yaw, pitch = (math.radians(v) for v in self.pose)
        k = 0.82  # sphere radius relative to the oval

        def screen(x, y):
            # Mirrored: +yaw (user's left) moves the mesh to screen-left.
            return cx - x * rx * k, cy - y * ry * k

        cr.set_line_width(1.4)
        lines = [[(lat, lon) for lat in range(-80, 81, 5)] for lon in MERIDIANS]
        lines += [[(lat, lon) for lon in range(-90, 91, 5)] for lat in PARALLELS]
        for i, line in enumerate(lines):
            main = i < len(MERIDIANS) and MERIDIANS[i] == 0
            prev = None
            for lat, lon in line:
                x, y, z = _project(lat, lon, yaw, pitch)
                pt = screen(x, y)
                if prev is not None and z > 0 and prev[1] > 0:
                    a = 0.15 + 0.45 * min(z, prev[1])
                    if main:
                        cr.set_source_rgba(accent.red, accent.green, accent.blue, min(1.0, a + 0.35))
                    else:
                        cr.set_source_rgba(1, 1, 1, a)
                    cr.move_to(*prev[0])
                    cr.line_to(*pt)
                    cr.stroke()
                prev = (pt, z)

    def _draw_contour(self, snapshot, w, h, cx, cy, rx, ry):
        cr = snapshot.append_cairo(Graphene.Rect().init(0, 0, w, h))
        fg = self.get_color()
        accent = Adw.StyleManager.get_default().get_accent_color_rgba()
        cr.set_line_cap(1)  # round

        # Base track.
        cr.set_line_width(5)
        cr.set_source_rgba(0.5, 0.5, 0.5, 0.35)  # neutral: readable in light and dark
        _ellipse_arc(cr, cx, cy, rx, ry, 0, 360)
        cr.stroke()

        cr.set_line_width(7)
        # Reference pose: fills clockwise from the top.
        if self.calibration is not None and not self.front_done:
            cr.set_source_rgba(accent.red, accent.green, accent.blue, 0.95)
            _ellipse_arc(cr, cx, cy, rx, ry, -90, -90 + 360 * min(self.calibration, 1.0))
            cr.stroke()
        elif self.front_done and not self.state and not self.active:
            cr.set_source_rgba(SUCCESS.red, SUCCESS.green, SUCCESS.blue, 0.8)
            _ellipse_arc(cr, cx, cy, rx, ry, 0, 360)
            cr.stroke()

        # Captured / skipped directions.
        for code, st in self.state.items():
            if code not in SECTORS:
                continue
            if st == "done":
                cr.set_source_rgba(SUCCESS.red, SUCCESS.green, SUCCESS.blue, 1)
            else:
                cr.set_source_rgba(fg.red, fg.green, fg.blue, 0.35)
            for a0, a1 in _sector_arcs(code):
                _ellipse_arc(cr, cx, cy, rx, ry, a0, a1)
                cr.stroke()

        # Live progress towards the active direction.
        if self.active and self.active[0] in SECTORS:
            code, _axis, target = self.active
            centre, _inner, half = SECTORS[code]
            frac = 0.0
            if target and self.value * target > 0:
                frac = min(abs(self.value) / abs(target), 1.0)
            if frac > 0.02:
                pulse = 0.7 + 0.3 * math.sin(GLib.get_monotonic_time() / 1e6 * 6)
                c = SUCCESS if self.on_target else accent
                cr.set_source_rgba(c.red, c.green, c.blue, pulse if self.on_target else 1.0)
                for a0, a1 in _sector_arcs(code, frac):
                    _ellipse_arc(cr, cx, cy, rx, ry, a0, a1)
                    cr.stroke()
            # Target marker: small ticks at the sector's ends.
            cr.set_line_width(3)
            cr.set_source_rgba(accent.red, accent.green, accent.blue, 0.6)
            for a in (centre - half, centre + half):
                ra = math.radians(a)
                for r0, r1 in ((1.06, 1.12),):
                    cr.move_to(cx + rx * r0 * math.cos(ra), cy + ry * r0 * math.sin(ra))
                    cr.line_to(cx + rx * r1 * math.cos(ra), cy + ry * r1 * math.sin(ra))
            cr.stroke()

    # -- full-frame mode with detection overlay ------------------------------

    def _snapshot_full(self, snapshot):
        w, h = self.get_width(), self.get_height()
        tw, th = self.tex_size
        scale = min(w / tw, h / th)
        dw, dh = tw * scale, th * scale
        ox, oy = (w - dw) / 2, (h - dh) / 2
        rect = Graphene.Rect().init(ox, oy, dw, dh)
        clip = Gsk.RoundedRect()
        clip.init_from_rect(rect, 12)
        snapshot.push_rounded_clip(clip)
        snapshot.append_color(Gdk.RGBA(red=0.08, green=0.08, blue=0.09, alpha=1), rect)
        if self.texture:
            snapshot.save()
            p = Graphene.Point()
            snapshot.translate(p.init(ox + dw, oy))
            snapshot.scale(-scale, scale)  # mirrored
            snapshot.append_scaled_texture(
                self.texture, Gsk.ScalingFilter.LINEAR, Graphene.Rect().init(0, 0, tw, th)
            )
            snapshot.restore()
        snapshot.pop()
        if len(self.face) < 14:
            return

        def to_screen(x, y):
            return ox + (tw - x) * scale, oy + y * scale

        cr = snapshot.append_cairo(Graphene.Rect().init(0, 0, w, h))
        accent = Adw.StyleManager.get_default().get_accent_color_rgba()
        x, y, fw, fh = self.face[:4]
        (x0, y0), (x1, y1) = to_screen(x, y), to_screen(x + fw, y + fh)
        cr.set_source_rgba(accent.red, accent.green, accent.blue, 0.9)
        cr.set_line_width(2)
        _rounded_rect(cr, min(x0, x1), y0, abs(x1 - x0), y1 - y0, 10)
        cr.stroke()
        colors = [(0.21, 0.52, 0.89), (0.21, 0.52, 0.89), (0.96, 0.76, 0.07), (0.88, 0.11, 0.14), (0.88, 0.11, 0.14)]
        pts = [to_screen(self.face[4 + 2 * i], self.face[5 + 2 * i]) for i in range(5)]
        cr.set_source_rgba(1, 1, 1, 0.45)
        cr.set_line_width(1.2)
        for a, b in ((0, 1), (0, 2), (1, 2), (3, 4), (2, 3), (2, 4)):
            cr.move_to(*pts[a])
            cr.line_to(*pts[b])
        cr.stroke()
        for (px, py), (r, g, b) in zip(pts, colors):
            cr.set_source_rgba(1, 1, 1, 0.95)
            cr.arc(px, py, 5, 0, 2 * math.pi)
            cr.fill()
            cr.set_source_rgba(r, g, b, 1)
            cr.arc(px, py, 3.5, 0, 2 * math.pi)
            cr.fill()


def _stop(offset, r, g, b, a):
    s = Gsk.ColorStop()
    s.offset = offset
    s.color = Gdk.RGBA(red=r, green=g, blue=b, alpha=a)
    return s


def _rounded_rect(cr, x, y, w, h, r):
    cr.new_sub_path()
    cr.arc(x + w - r, y + r, r, -math.pi / 2, 0)
    cr.arc(x + w - r, y + h - r, r, 0, math.pi / 2)
    cr.arc(x + r, y + h - r, r, math.pi / 2, math.pi)
    cr.arc(x + r, y + r, r, math.pi, 3 * math.pi / 2)
    cr.close_path()

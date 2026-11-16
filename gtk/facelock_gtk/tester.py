"""Live test page: full IR frame with detected box and landmarks, live
similarity / liveness readouts, and a real authentication attempt."""

from gettext import gettext as _

from gi.repository import Adw, GLib, Gtk

from .camera_view import CameraView
from .daemon import error_text


def reason_text(code):
    return {
        "ok": _("Recognized"),
        "no-profile": _("No face enrolled"),
        "no-face": _("No face detected"),
        "no-match": _("Face not recognized"),
        "no-ir-strobe": _("The IR emitter is not blinking"),
        "low-ir": _("Too little infrared reflected (screen? too far?)"),
        "spoof": _("Looks like a photo or a screen"),
        "no-antispoof": _("Photo check models missing for the colour webcam"),
        "eyewear-mismatch": _("Eye reflections don't match the enrolled face"),
        "eyewear-undecided": _("Look straight at the screen"),
        "cooldown": _("Too many failures, try again later"),
        "camera": _("Camera unavailable"),
    }.get(code, code)


class TestPage(Adw.NavigationPage):
    def __init__(self, daemon, toast_overlay):
        super().__init__(title=_("Test Recognition"), tag="test")
        self.daemon = daemon
        self.toasts = toast_overlay
        self.handlers = []
        self.active = False

        v = daemon.proxy.get_cached_property("Threshold") if daemon.proxy else None
        self.threshold = v.unpack() if v else 0.7

        self.view = CameraView(ring=False, full=True)
        self.view.set_size_request(-1, 220)
        legend = Gtk.Label(css_classes=["caption"], use_markup=True)
        legend.set_markup("   ".join(
            f'<span foreground="{c}">●</span> <span alpha="70%">{GLib.markup_escape_text(t)}</span>'
            for c, t in (("#3584e4", _("Eyes")), ("#f6d32d", _("Nose")), ("#e01b24", _("Mouth corners")))
        ))

        group = Adw.PreferencesGroup(title=_("Live Measurements"))
        self.sim_bar = Gtk.LevelBar(min_value=0, max_value=1, valign=Gtk.Align.CENTER, hexpand=True)
        self.sim_bar.set_size_request(120, -1)
        self.sim_bar.add_offset_value("low", max(self.threshold - 0.1, 0.01))
        self.sim_bar.add_offset_value("high", self.threshold)
        self.sim_bar.add_offset_value("full", 1.0)
        self.sim_label = Gtk.Label(width_chars=5, xalign=1, css_classes=["numeric"])
        self.sim_row = Adw.ActionRow(
            title=_("Similarity"),
            subtitle=_("Needs at least {t:.2f} to unlock").format(t=self.threshold),
        )
        self.sim_row.add_suffix(self.sim_bar)
        self.sim_row.add_suffix(self.sim_label)
        group.add(self.sim_row)

        self.ir_row = self._value_row(group, _("Infrared Reflection"), _("Light from the emitter bouncing off your face"))
        # Colour webcams only, instead of the IR row.
        self.real_row = self._value_row(group, _("Live Face"), _("Chance that this is not a photo or a screen"))
        self.real_row.set_visible(False)
        self.pose_row = self._value_row(group, _("Head Position"), _("Left/right and up/down angle"))
        self.eyes_row = self._value_row(group, _("Eye Reflections"), _("What the emitter's glints suggest"))

        self.test_btn = Gtk.Button(label=_("Test Authentication"), css_classes=["pill", "suggested-action"], halign=Gtk.Align.CENTER)
        self.test_btn.connect("clicked", self._on_test)
        self.result_icon = Gtk.Image(css_classes=["tile-icon"], valign=Gtk.Align.CENTER)
        self.result = Adw.ActionRow(title=" ")
        self.result.add_prefix(self.result_icon)
        self.result_group = Adw.PreferencesGroup(visible=False)
        self.result_group.add(self.result)

        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=18, margin_top=12, margin_bottom=24, margin_start=12, margin_end=12)
        box.append(self.view)
        box.append(legend)
        box.append(self.result_group)
        box.append(self.test_btn)
        box.append(group)
        scroll = Gtk.ScrolledWindow(child=Adw.Clamp(maximum_size=560, child=box), hscrollbar_policy=Gtk.PolicyType.NEVER)

        tv = Adw.ToolbarView(content=scroll)
        tv.add_top_bar(Adw.HeaderBar())
        self.set_child(tv)
        self.connect("shown", lambda *_: self._start())
        self.connect("hidden", lambda *_: self._stop())

    def _value_row(self, group, title, subtitle):
        row = Adw.ActionRow(title=title, subtitle=subtitle)
        label = Gtk.Label(css_classes=["numeric", "dim-label"])
        row.add_suffix(label)
        group.add(row)
        row.value = label
        return row

    def _start(self):
        if self.active:
            return
        self.active = True
        self.handlers = [
            self.daemon.connect("frame", self._on_frame),
            self.daemon.connect("stats", self._on_stats),
        ]
        self.daemon.start_preview(self._preview_started)

    def _preview_started(self, _res, err):
        if err:
            self.toasts.add_toast(Adw.Toast(title=error_text(err)))

    def _stop(self):
        if not self.active:
            return
        self.active = False
        for h in self.handlers:
            self.daemon.disconnect(h)
        self.handlers = []
        self.daemon.cancel()

    def _on_frame(self, _d, w, h, pixels, face):
        self.view.set_frame(w, h, pixels, face)
        if len(face) < 4:
            self.sim_label.set_label("—")
            self.sim_bar.set_value(0)

    def _on_stats(self, _d, sim, gain, real, yaw, pitch, eyewear):
        sim = max(sim, 0.0)
        self.sim_bar.set_value(min(sim, 1.0))
        self.sim_label.set_label(f"{sim:.2f}")
        colour = real >= 0
        self.ir_row.set_visible(not colour)
        self.eyes_row.set_visible(not colour)
        self.real_row.set_visible(colour)
        self.ir_row.value.set_label(f"{gain:.0f} / 255")
        self.real_row.value.set_label(f"{real * 100:.0f} %" if colour else "")
        # yaw > 0: the user turned to their left.
        side = _("left") if yaw > 3 else _("right") if yaw < -3 else _("center")
        self.pose_row.value.set_label(f"{abs(yaw):.0f}° {side} · {pitch:+.0f}°")
        self.eyes_row.value.set_label(
            {"glasses": _("Glasses"), "none": _("Bare eyes")}.get(eyewear, _("Undecided"))
        )

    def _on_test(self, _btn):
        # Verify takes over the camera (the daemon stops our preview).
        self.test_btn.set_sensitive(False)
        self._show_result("camera-web-symbolic", "accent", _("Look at the camera…"), "")
        self.daemon.verify(self._verified)

    def _show_result(self, icon, style, title, subtitle):
        self.result_icon.set_from_icon_name(icon)
        for c in ("accent", "success", "error"):
            self.result_icon.remove_css_class(c)
        self.result_icon.add_css_class(style)
        self.result.set_title(GLib.markup_escape_text(title))
        self.result.set_subtitle(GLib.markup_escape_text(subtitle))
        self.result_group.set_visible(True)

    def _verified(self, res, err):
        self.test_btn.set_sensitive(True)
        if err:
            self._show_result("dialog-error-symbolic", "error", _("Test failed"), error_text(err))
        else:
            ok, reason = res
            if ok:
                self._show_result("object-select-symbolic", "success", _("Unlocked"), reason_text(reason))
            else:
                self._show_result("window-close-symbolic", "error", _("Not unlocked"), reason_text(reason))
        # Resume the live preview.
        if self.active:
            GLib.timeout_add(300, lambda: (self.daemon.start_preview(self._preview_started), False)[1])

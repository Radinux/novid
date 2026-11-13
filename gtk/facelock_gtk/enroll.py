"""Guided enrollment page: round preview, progress ring, live instructions."""

from gettext import gettext as _

from gi.repository import Adw, GLib, GObject, Gtk

from .camera_view import CameraView
from .daemon import error_code, error_text

INSTRUCTIONS = {
    "look-front": lambda: _("Look straight at the screen"),
    "recenter": lambda: _("Back to the center, facing the screen"),
    "turn-left": lambda: _("Slowly turn your head to the left"),
    "turn-left-more": lambda: _("A little more to the left"),
    "turn-right": lambda: _("Now turn to the right"),
    "turn-right-more": lambda: _("A little more to the right"),
    "look-up": lambda: _("Back to the center, and lift your chin"),
    "look-down": lambda: _("Now lower your head a little"),
}


def hint_text(code, axis):
    return {
        "no-face": _("Face lost — look at the camera"),
        "more": _("A bit more…"),
        "hold": _("Hold still…"),
        "overshoot": _("A bit too far, come back slightly"),
        "wrong-way": _("Other direction"),
        "center": _("Back to the center"),
        "straighten": _("Keep your head level") if axis == "yaw" else _("Keep your head centered"),
    }.get(code, "")


class EnrollPage(Adw.NavigationPage):
    __gsignals__ = {
        # Emitted when the user wants to try the new face right away.
        "test-requested": (GObject.SignalFlags.RUN_FIRST, None, ()),
        "finished": (GObject.SignalFlags.RUN_FIRST, None, ()),
    }

    def __init__(self, daemon, label):
        super().__init__(title=_("Add a Face"), tag="enroll")
        self.daemon = daemon
        self.label = label
        self.axis = "yaw"
        self.running = False
        self.handlers = []

        self.stack = Gtk.Stack(transition_type=Gtk.StackTransitionType.CROSSFADE)

        # Waiting for polkit / the camera.
        wait = Adw.StatusPage(
            title=_("Waiting for authorization…"),
            description=_("Confirm with your password to add a face."),
            paintable=Adw.SpinnerPaintable(),
        )
        self.stack.add_named(wait, "wait")

        # Live enrollment.
        self.view = CameraView(ring=True)
        self.counter = Gtk.Label(css_classes=["dim-label", "caption-heading"])
        self.instruction = Gtk.Label(css_classes=["title-2"], wrap=True, justify=Gtk.Justification.CENTER)
        self.hint = Gtk.Label(css_classes=["dim-label"], wrap=True, justify=Gtk.Justification.CENTER)
        self.hint.set_size_request(-1, 24)
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12, margin_top=12, margin_bottom=24)
        box.append(self.view)
        box.append(self.counter)
        box.append(self.instruction)
        box.append(self.hint)
        self.stack.add_named(Adw.Clamp(maximum_size=460, child=box, margin_start=12, margin_end=12), "run")

        # Success.
        test_btn = Gtk.Button(label=_("Test Recognition"), css_classes=["pill", "suggested-action"])
        test_btn.connect("clicked", lambda *_: self.emit("test-requested"))
        done_btn = Gtk.Button(label=_("Done"), css_classes=["pill"])
        done_btn.connect("clicked", lambda *_: self.emit("finished"))
        btns = Gtk.Box(spacing=12, halign=Gtk.Align.CENTER)
        btns.append(done_btn)
        btns.append(test_btn)
        self.done_page = Adw.StatusPage(icon_name="emblem-ok-symbolic", title=_("Face Added"), child=btns)
        self.done_page.add_css_class("success")
        self.stack.add_named(self.done_page, "done")

        # Failure.
        retry = Gtk.Button(label=_("Try Again"), css_classes=["pill", "suggested-action"], halign=Gtk.Align.CENTER)
        retry.connect("clicked", lambda *_: self.start())
        self.error_page = Adw.StatusPage(icon_name="dialog-warning-symbolic", child=retry)
        self.stack.add_named(self.error_page, "error")

        tv = Adw.ToolbarView(content=self.stack)
        tv.add_top_bar(Adw.HeaderBar())
        self.set_child(tv)
        self.connect("hidden", self._on_hidden)

    def start(self):
        self.view.reset()
        self.stack.set_visible_child_name("wait")
        self._connect_signals()
        self.running = True
        self.daemon.start_enroll(self.label, self._started)

    def _started(self, _res, err):
        if err:
            self.running = False
            self._disconnect_signals()
            code = error_code(err)
            if code.endswith("AccessDenied"):
                self._fail(_("Authorization denied"), _("Adding a face requires your password."))
            elif code.endswith("LimitsExceeded"):
                self._fail(_("Camera busy"), _("Another face operation is in progress."))
            else:
                self._fail(_("Could not start"), error_text(err))

    def _fail(self, title, desc):
        self.error_page.set_title(title)
        self.error_page.set_description(desc)
        self.stack.set_visible_child_name("error")

    def _connect_signals(self):
        d = self.daemon
        self.handlers = [
            d.connect("frame", self._on_frame),
            d.connect("enroll-calibrating", self._on_calibrating),
            d.connect("enroll-step", self._on_step),
            d.connect("enroll-progress", self._on_progress),
            d.connect("enroll-captured", self._on_captured),
            d.connect("enroll-skipped", self._on_skipped),
            d.connect("enroll-finished", self._on_finished),
        ]

    def _disconnect_signals(self):
        for h in self.handlers:
            self.daemon.disconnect(h)
        self.handlers = []

    def _on_hidden(self, *_):
        if self.running:
            self.daemon.cancel()
            self.running = False
        self._disconnect_signals()

    # -- daemon signals -----------------------------------------------------

    def _on_frame(self, _d, w, h, pixels, face):
        if self.stack.get_visible_child_name() == "wait":
            self.stack.set_visible_child_name("run")
            self.instruction.set_label(_("Look straight at the screen"))
            self.counter.set_label(_("Measuring your reference position"))
        self.view.set_frame(w, h, pixels, face)

    def _on_calibrating(self, _d, progress):
        self.view.calibration = progress

    def _on_step(self, _d, index, total, code, axis, _target):
        self.axis = axis
        self.counter.set_label(_("Step {n} of {total}").format(n=index + 1, total=total))
        self.instruction.set_label(INSTRUCTIONS.get(code, lambda: code)())
        self.hint.set_label("")
        self.current = code
        self.view.active = None if code == "look-front" else (code, axis, _target)
        self.view.value = 0.0
        self.view.on_target = False

    def _on_progress(self, _d, value, off, hint):
        self.hint.set_label(hint_text(hint, self.axis))
        if hint == "no-face":
            return
        self.view.value = value
        self.view.on_target = hint == "hold"
        yaw, pitch = (value, off) if self.axis == "yaw" else (off, value)
        self.view.set_pose(yaw, pitch)

    def _on_captured(self, _d, _index):
        if self.current == "look-front":
            self.view.front_done = True
        else:
            self.view.state[self.current] = "done"
        self.view.active = None
        self.hint.set_label("✓")

    def _on_skipped(self, _d, _index):
        if self.current != "look-front":
            self.view.state[self.current] = "skipped"
        self.view.active = None

    def _on_finished(self, _d, ok, _profile_id, error):
        self.running = False
        self._disconnect_signals()
        if ok:
            self.done_page.set_description(_("You can now unlock with “{label}”.").format(label=self.label))
            self.stack.set_visible_child_name("done")
            return
        if error == "cancelled":
            return
        reasons = {
            "too-few-poses": _("Not enough head positions were captured. Move slowly and pause at each position."),
            "timeout": _("This took too long."),
        }
        self._fail(_("Face Not Added"), reasons.get(error, error))

"""Settings: camera. (Where the face is used lives on the main window tiles.)"""

from gettext import gettext as _

from gi.repository import Adw, GLib, Gtk

from .daemon import error_code, error_text


def _tile(icon_name):
    return Gtk.Image(icon_name=icon_name, css_classes=["tile-icon", "neutral"], valign=Gtk.Align.CENTER)


class SettingsDialog(Adw.PreferencesDialog):
    def __init__(self, daemon):
        super().__init__(title=_("Settings"), search_enabled=False)
        self.daemon = daemon
        self._guard = False

        page = Adw.PreferencesPage()
        self.add(page)

        cam = Adw.PreferencesGroup(title=_("Camera"))
        self.camera_row = Adw.ActionRow(title=_("Camera"))
        self.camera_row.add_prefix(_tile("camera-web-symbolic"))
        cam.add(self.camera_row)
        self.rgb_row = Adw.SwitchRow(
            title=_("Allow Regular Webcams"),
            subtitle=_("Only used when there is no infrared camera"),
        )
        self.rgb_row.add_prefix(_tile("camera-photo-symbolic"))
        self.rgb_row.connect("notify::active", self._on_rgb_toggled)
        cam.add(self.rgb_row)
        page.add(cam)

        self.refresh()

    def _set(self, row, active):
        self._guard = True
        row.set_active(active)
        self._guard = False

    def refresh(self):
        self.daemon.get_system_settings(self._got_settings)

    def _got_settings(self, settings, err):
        if err:
            self.add_toast(Adw.Toast(title=error_text(err)))
            return
        self._set(self.rgb_row, bool(settings.get("allow-rgb")))
        name, kind = self.daemon.prop("CameraName"), self.daemon.prop("CameraKind")
        self.camera_row.set_title(GLib.markup_escape_text(name or _("No camera found")))
        self.camera_row.remove_css_class("warning")
        if kind == "ir":
            self.camera_row.set_subtitle(_("Infrared camera — protected against photos and screens"))
        elif kind == "rgb":
            self.camera_row.set_subtitle(_("Regular webcam — weaker protection; the keyring is never unlocked"))
            self.camera_row.add_css_class("warning")
        else:
            self.camera_row.set_subtitle(_("Connect an infrared camera, or allow regular webcams below"))

    def _error(self, err):
        if error_code(err).endswith("AccessDenied"):
            self.add_toast(Adw.Toast(title=_("Authorization denied")))
        else:
            self.add_toast(Adw.Toast(title=error_text(err)))

    def _on_rgb_toggled(self, row, _pspec):
        if self._guard:
            return
        if not row.get_active():
            self.daemon.set_allow_rgb(False, self._rgb_done(False))
            return
        d = Adw.AlertDialog(
            heading=_("Allow Regular Webcams?"),
            body=_(
                "Without an infrared camera, a good photo or video of you may be able to unlock this computer. "
                "Your keyring will never be unlocked this way."
            ),
        )
        d.add_response("cancel", _("Cancel"))
        d.add_response("ok", _("Allow"))
        d.set_response_appearance("ok", Adw.ResponseAppearance.DESTRUCTIVE)
        d.set_close_response("cancel")

        def on_response(_d, r):
            if r == "ok":
                self.daemon.set_allow_rgb(True, self._rgb_done(True))
            else:
                self._set(row, False)

        d.connect("response", on_response)
        d.present(self)

    def _rgb_done(self, on):
        def done(_r, err):
            if err:
                self._set(self.rgb_row, not on)
                self._error(err)
                return
            # The service restarts to apply it; ask again once it's back.
            GLib.timeout_add(1500, lambda: (self.refresh(), False)[1])

        return done

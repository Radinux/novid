"""Thin GObject wrapper around the org.facelock.Daemon1 system service.

Every call is asynchronous so the UI never blocks, including while polkit
shows its password dialog. Daemon signals are re-emitted as GObject signals.
"""

from gi.repository import Gio, GLib, GObject

BUS_NAME = "org.facelock.Daemon1"
PATH = "/org/facelock/Daemon1"
IFACE = "org.facelock.Daemon1"
# Long enough for a polkit password prompt.
CALL_TIMEOUT_MS = 10 * 60 * 1000


class Profile:
    def __init__(self, t):
        self.id, self.label, self.created, self.eyewear, self.samples, self.usable = t


def error_code(err: GLib.Error) -> str:
    """D-Bus error name of a remote error, or '' for local errors."""
    return Gio.DBusError.get_remote_error(err) or ""


def error_text(err: GLib.Error) -> str:
    Gio.DBusError.strip_remote_error(err)
    return err.message


class Daemon(GObject.Object):
    __gsignals__ = {
        "frame": (GObject.SignalFlags.RUN_FIRST, None, (int, int, GLib.Bytes, object)),
        "stats": (GObject.SignalFlags.RUN_FIRST, None, (float, float, float, float, str)),
        "enroll-calibrating": (GObject.SignalFlags.RUN_FIRST, None, (float,)),
        "enroll-step": (GObject.SignalFlags.RUN_FIRST, None, (int, int, str, str, float)),
        "enroll-progress": (GObject.SignalFlags.RUN_FIRST, None, (float, float, str)),
        "enroll-captured": (GObject.SignalFlags.RUN_FIRST, None, (int,)),
        "enroll-skipped": (GObject.SignalFlags.RUN_FIRST, None, (int,)),
        "enroll-finished": (GObject.SignalFlags.RUN_FIRST, None, (bool, str, str)),
    }

    def __init__(self):
        super().__init__()
        self.user = GLib.get_user_name()
        self.proxy = None

    def connect_bus(self, callback):
        """callback(ok: bool, message: str)."""

        def done(_src, res):
            try:
                self.proxy = Gio.DBusProxy.new_for_bus_finish(res)
            except GLib.Error as e:
                callback(False, error_text(e))
                return
            self.proxy.connect("g-signal", self._on_signal)
            # Triggers D-Bus activation and checks the daemon is really there.
            self.proxy.call(
                "org.freedesktop.DBus.Peer.Ping", None, Gio.DBusCallFlags.NONE, 15000, None,
                lambda p, r: self._ping_done(p, r, callback),
            )

        Gio.DBusProxy.new_for_bus(
            Gio.BusType.SYSTEM, Gio.DBusProxyFlags.NONE, None, BUS_NAME, PATH, IFACE, None, done
        )

    def _ping_done(self, proxy, res, callback):
        try:
            proxy.call_finish(res)
            callback(True, "")
        except GLib.Error as e:
            callback(False, error_text(e))

    @property
    def model_id(self):
        v = self.proxy.get_cached_property("ModelId") if self.proxy else None
        return v.unpack() if v else ""

    def prop(self, name, default=""):
        v = self.proxy.get_cached_property(name) if self.proxy else None
        return v.unpack() if v is not None else default

    # -- method calls -------------------------------------------------------

    def _call(self, method, args, callback=None, unpack=None):
        """callback(result, error) where error is a GLib.Error or None."""
        flags = Gio.DBusCallFlags.ALLOW_INTERACTIVE_AUTHORIZATION

        def done(proxy, res):
            try:
                v = proxy.call_finish(res)
            except GLib.Error as e:
                if callback:
                    callback(None, e)
                return
            if callback:
                callback(unpack(v) if unpack else None, None)

        self.proxy.call(method, args, flags, CALL_TIMEOUT_MS, None, done)

    def list_profiles(self, callback):
        self._call(
            "ListProfiles", GLib.Variant("(s)", (self.user,)), callback,
            lambda v: [Profile(t) for t in v.unpack()[0]],
        )

    def start_enroll(self, label, callback):
        self._call("StartEnroll", GLib.Variant("(ss)", (self.user, label)), callback)

    def start_preview(self, callback):
        self._call("StartPreview", GLib.Variant("(s)", (self.user,)), callback)

    def cancel(self):
        if self.proxy:
            self._call("Cancel", None)

    def verify(self, callback):
        self._call("Verify", GLib.Variant("(s)", (self.user,)), callback, lambda v: v.unpack())

    def delete_profile(self, pid, callback):
        self._call("DeleteProfile", GLib.Variant("(ss)", (self.user, pid)), callback)

    def get_system_settings(self, callback):
        self._call("GetSystemSettings", None, callback, lambda v: v.unpack()[0])

    def set_pam_enabled(self, service, enabled, callback):
        self._call("SetPamEnabled", GLib.Variant("(sb)", (service, enabled)), callback)

    def set_allow_rgb(self, enabled, callback):
        self._call("SetAllowRgb", GLib.Variant("(b)", (enabled,)), callback)

    def has_keyring_password(self, callback):
        self._call("HasKeyringPassword", GLib.Variant("(s)", (self.user,)), callback, lambda v: v.unpack()[0])

    def set_keyring_password(self, password, callback):
        self._call("SetKeyringPassword", GLib.Variant("(ss)", (self.user, password)), callback)

    def clear_keyring_password(self, callback):
        self._call("ClearKeyringPassword", GLib.Variant("(s)", (self.user,)), callback)

    def rename_profile(self, pid, label, callback):
        self._call("RenameProfile", GLib.Variant("(sss)", (self.user, pid, label)), callback)

    # -- signals ------------------------------------------------------------

    def _on_signal(self, _proxy, _sender, name, params):
        if name == "PreviewFrame":
            w, h = params.get_child_value(0).get_uint32(), params.get_child_value(1).get_uint32()
            # get_data_as_bytes avoids unpacking ~57 kB into a Python list per frame.
            pixels = params.get_child_value(2).get_data_as_bytes()
            face = params.get_child_value(3).unpack()
            self.emit("frame", w, h, pixels, face)
        elif name == "PreviewStats":
            self.emit("stats", *params.unpack())
        elif name == "EnrollCalibrating":
            self.emit("enroll-calibrating", *params.unpack())
        elif name == "EnrollStep":
            self.emit("enroll-step", *params.unpack())
        elif name == "EnrollProgress":
            self.emit("enroll-progress", *params.unpack())
        elif name == "EnrollCaptured":
            self.emit("enroll-captured", *params.unpack())
        elif name == "EnrollSkipped":
            self.emit("enroll-skipped", *params.unpack())
        elif name == "EnrollFinished":
            self.emit("enroll-finished", *params.unpack())

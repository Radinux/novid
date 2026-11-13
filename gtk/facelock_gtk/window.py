"""Main window: a dashboard. Coloured tiles switch face unlock on and off per
service, enrolled faces are cards, and the live test is one click away."""

from gettext import gettext as _

from gi.repository import Adw, Gio, GLib, Gtk, Pango

from .daemon import Daemon, error_code, error_text
from .enroll import EnrollPage
from .tester import TestPage

APP_ID = "org.facelock.Facelock"

# (key, icon, title, colour class). Keys "pam-*" match GetSystemSettings.
TILES = [
    ("pam-sudo", "utilities-terminal-symbolic", lambda: _("Terminal (sudo)"), "blue"),
    ("pam-gdm-password", "system-lock-screen-symbolic", lambda: _("Login and Lock Screen"), "purple"),
    ("pam-polkit-1", "security-high-symbolic", lambda: _("Administrator Prompts"), "orange"),
    ("keyring", "facelock-keyring-symbolic", lambda: _("Keyring"), "green"),
]


class Tile(Gtk.Button):
    """Quick-settings style toggle: icon, title, state line."""

    def __init__(self, icon, title, colour):
        super().__init__(css_classes=["service-tile", colour], hexpand=True)
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=4)
        top = Gtk.Box(spacing=8, margin_bottom=8)
        top.append(Gtk.Image(icon_name=icon, css_classes=["tile-icon"], halign=Gtk.Align.START))
        self.switch = Gtk.Box(css_classes=["m3-switch"], valign=Gtk.Align.START, halign=Gtk.Align.END, hexpand=True)
        self.knob = Gtk.Box(css_classes=["knob"], valign=Gtk.Align.CENTER, halign=Gtk.Align.START, hexpand=True)
        self.switch.append(self.knob)
        top.append(self.switch)
        box.append(top)
        box.append(Gtk.Label(label=title, css_classes=["heading"], xalign=0, wrap=True,
                             wrap_mode=Pango.WrapMode.WORD_CHAR, lines=2, ellipsize=Pango.EllipsizeMode.END))
        self.state = Gtk.Label(css_classes=["caption", "tile-state"], xalign=0, ellipsize=Pango.EllipsizeMode.END)
        box.append(self.state)
        self.set_child(box)
        self.active = False

    def update(self, active, text):
        self.active = active
        (self.add_css_class if active else self.remove_css_class)("on")
        self.knob.set_halign(Gtk.Align.END if active else Gtk.Align.START)
        self.state.set_label(text)


class Window(Adw.ApplicationWindow):
    def __init__(self, app):
        super().__init__(application=app, title=_("NovID"), default_width=560, default_height=800)
        self.add_css_class("m3")
        style = Adw.StyleManager.get_default()
        style.connect("notify::dark", lambda sm, _p: self._sync_dark(sm))
        self._sync_dark(style)
        self.set_size_request(360, 520)
        self.daemon = Daemon()
        self.profiles = []
        self.settings = {}
        self.keyring = False
        self.busy = set()

        self.toasts = Adw.ToastOverlay()
        self.nav = Adw.NavigationView()
        self.toasts.set_child(self.nav)
        self.set_content(self.toasts)

        self.stack = Gtk.Stack(transition_type=Gtk.StackTransitionType.CROSSFADE)
        self.stack.add_named(Adw.StatusPage(paintable=Adw.SpinnerPaintable()), "loading")

        retry = Gtk.Button(label=_("Retry"), css_classes=["pill"], halign=Gtk.Align.CENTER)
        retry.connect("clicked", lambda *_: self.load())
        self.unavailable = Adw.StatusPage(
            icon_name="dialog-warning-symbolic",
            title=_("Service Not Available"),
            description=_("The facelock service could not be reached. Install the “facelock” package, then try again."),
            child=retry,
        )
        self.stack.add_named(self.unavailable, "unavailable")

        add_first = Gtk.Button(label=_("Add a Face"), css_classes=["pill", "suggested-action"], halign=Gtk.Align.CENTER)
        add_first.connect("clicked", lambda *_: self.ask_label())
        empty = Adw.StatusPage(
            icon_name=APP_ID,
            title=_("Unlock With Your Face"),
            description=_("Use the infrared camera to unlock sudo, the lock screen and system prompts."),
            child=add_first,
            css_classes=["hero-status"],
        )
        self.stack.add_named(empty, "empty")
        self.stack.add_named(self._build_dashboard(), "list")

        menu = Gtk.MenuButton(icon_name="open-menu-symbolic", menu_model=self._menu(), primary=True)
        header = Adw.HeaderBar(show_title=False)
        header.pack_end(menu)
        tv = Adw.ToolbarView(content=self.stack, top_bar_style=Adw.ToolbarStyle.FLAT)
        tv.add_top_bar(header)
        self.nav.add(Adw.NavigationPage(title=_("NovID"), child=tv, tag="main"))

        for name, cb in (
            ("settings", lambda *_: self.open_settings()),
            ("add-face", lambda *_: self.ask_label()),
            ("test", lambda *_: self.open_test()),
        ):
            act = Gio.SimpleAction.new(name, None)
            act.connect("activate", cb)
            self.add_action(act)
        app.set_accels_for_action("win.settings", ["<Control>comma"])
        app.set_accels_for_action("win.add-face", ["<Control>n"])
        app.set_accels_for_action("win.test", ["<Control>t"])

        # No focus ring on the first tile at launch; Tab brings it back.
        self.connect("map", lambda *_: GLib.idle_add(self._clear_focus))
        self.daemon.connect_bus(self._bus_ready)

    def _sync_dark(self, style):
        (self.add_css_class if style.get_dark() else self.remove_css_class)("dark")

    def _clear_focus(self):
        self.set_focus(None)
        self.set_focus_visible(False)
        return GLib.SOURCE_REMOVE

    # -- layout -------------------------------------------------------------

    def _section(self, title, suffix=None):
        box = Gtk.Box(spacing=6, margin_start=4, margin_end=4)
        box.append(Gtk.Label(label=title, css_classes=["m3-section"], xalign=0, hexpand=True))
        if suffix:
            box.append(suffix)
        return box

    def _build_dashboard(self):
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12, margin_bottom=24)

        # Header: icon, title, camera state.
        head = Gtk.Box(spacing=16, margin_bottom=14, margin_start=4)
        head.append(Gtk.Image(icon_name=APP_ID, pixel_size=72, css_classes=["hero-icon"]))
        titles = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=4, valign=Gtk.Align.CENTER)
        titles.append(Gtk.Label(label=_("NovID"), css_classes=["m3-headline"], xalign=0, wrap=True))
        cam = Gtk.Box(spacing=8, css_classes=["m3-chip"], halign=Gtk.Align.START)
        self.cam_dot = Gtk.Box(css_classes=["status-dot"], valign=Gtk.Align.CENTER)
        self.cam_label = Gtk.Label(xalign=0, wrap=True)
        cam.append(self.cam_dot)
        cam.append(self.cam_label)
        titles.append(cam)
        head.append(titles)
        box.append(head)

        # Where the face is used.
        box.append(self._section(_("Use Your Face For")))
        grid = Gtk.Grid(column_spacing=10, row_spacing=10, column_homogeneous=True, margin_bottom=18)
        self.tiles = {}
        for i, (key, icon, title, colour) in enumerate(TILES):
            tile = Tile(icon, title(), colour)
            tile.connect("clicked", self._on_tile, key)
            grid.attach(tile, i % 2, i // 2, 1, 1)
            self.tiles[key] = tile
        box.append(grid)

        # Faces.
        add = Gtk.Button(
            child=Adw.ButtonContent(icon_name="list-add-symbolic", label=_("Add")),
            css_classes=["flat"], action_name="win.add-face", tooltip_text=_("Add a Face"),
        )
        box.append(self._section(_("Your Faces"), add))
        self.faces = Gtk.FlowBox(
            selection_mode=Gtk.SelectionMode.NONE, homogeneous=True,
            min_children_per_line=2, max_children_per_line=4,
            column_spacing=10, row_spacing=10,
        )
        box.append(self.faces)
        self.faces_hint = Gtk.Label(css_classes=["dim-label", "caption"], xalign=0, wrap=True, margin_start=4)
        box.append(self.faces_hint)

        # Test.
        test = Gtk.Button(
            child=Adw.ButtonContent(icon_name="camera-web-symbolic", label=_("Test Recognition")),
            css_classes=["pill", "suggested-action", "test-button"],
            halign=Gtk.Align.CENTER, action_name="win.test", margin_top=22,
        )
        box.append(test)
        box.append(Gtk.Label(
            label=_("See what the camera detects and try unlocking"),
            css_classes=["dim-label", "caption"], justify=Gtk.Justification.CENTER, wrap=True,
        ))

        clamp = Adw.Clamp(maximum_size=600, child=box, margin_start=16, margin_end=16)
        return Gtk.ScrolledWindow(child=clamp, hscrollbar_policy=Gtk.PolicyType.NEVER)

    def _menu(self):
        m = Gio.Menu()
        m.append(_("Add a Face"), "win.add-face")
        m.append(_("Test Recognition"), "win.test")
        m.append(_("Camera Settings"), "win.settings")
        m.append(_("About NovID"), "app.about")
        return m

    def open_settings(self):
        if not self.daemon.proxy:
            return
        from .settings import SettingsDialog

        dialog = SettingsDialog(self.daemon)
        dialog.connect("closed", lambda *_: self._load_status())
        dialog.present(self)

    # -- data ---------------------------------------------------------------

    def _bus_ready(self, ok, message):
        if not ok:
            self.unavailable.set_description(
                _("The facelock service could not be reached. Install the “facelock” package, then try again.")
                + "\n\n" + message
            )
            self.stack.set_visible_child_name("unavailable")
            return
        self.daemon.connect("props-changed", lambda *_: self._update_status())
        self.load()

    def load(self):
        if not self.daemon.proxy:
            self.stack.set_visible_child_name("loading")
            self.daemon.connect_bus(self._bus_ready)
            return
        self.daemon.list_profiles(self._loaded)
        self._load_status()

    def _load_status(self):
        def got_settings(settings, err):
            if not err:
                self.settings = settings
                self._update_status()

        def got_keyring(has, err):
            if not err:
                self.keyring = bool(has)
                self._update_status()

        self.daemon.get_system_settings(got_settings)
        self.daemon.has_keyring_password(got_keyring)

    def _update_status(self):
        kind = self.daemon.prop("CameraKind")
        for c in ("ok", "warn", "bad"):
            self.cam_dot.remove_css_class(c)
        if kind == "ir":
            self.cam_dot.add_css_class("ok")
            self.cam_label.set_label(_("Infrared camera ready"))
        elif kind == "rgb":
            self.cam_dot.add_css_class("warn")
            self.cam_label.set_label(_("Regular webcam — weaker protection"))
        else:
            self.cam_dot.add_css_class("bad")
            self.cam_label.set_label(_("No camera found"))

        gdm = bool(self.settings.get("pam-gdm-password"))
        for key, tile in self.tiles.items():
            busy = key in self.busy
            if key == "keyring":
                tile.set_visible("pam-gdm-password" in self.settings and bool(self.settings.get("gdm-keyring")))
                on = self.keyring and gdm
                if busy:
                    text = _("Waiting…")
                elif kind == "rgb":
                    text = _("Needs an infrared camera")
                elif not gdm:
                    text = _("Turn on Login first")
                else:
                    text = _("Unlocked at login") if on else _("Asks for your password")
                tile.set_sensitive(not busy and (on or (gdm and kind != "rgb")))
            else:
                tile.set_visible(key in self.settings)  # PAM service not installed
                on = bool(self.settings.get(key))
                text = _("Waiting…") if busy else (_("On") if on else _("Off"))
                tile.set_sensitive(not busy)
            tile.update(on, text)

    def _loaded(self, profiles, err):
        if err:
            self.unavailable.set_description(error_text(err))
            self.stack.set_visible_child_name("unavailable")
            return
        self.profiles = profiles
        self.faces.remove_all()
        for p in profiles:
            self.faces.append(self._face_card(p))
        self.faces.append(self._add_card())
        if not any(p.eyewear == "glasses" for p in profiles):
            self.faces_hint.set_label(_("If you sometimes wear glasses, add one face with them and one without."))
            self.faces_hint.set_visible(True)
        else:
            self.faces_hint.set_visible(False)
        self.stack.set_visible_child_name("list" if profiles else "empty")

    def _face_card(self, p):
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6)
        box.append(Adw.Avatar(size=56, text=p.label, show_initials=True))
        box.append(Gtk.Label(label=p.label, css_classes=["heading"], ellipsize=Pango.EllipsizeMode.END, max_width_chars=14))
        if not p.usable:
            badge = Gtk.Label(label=_("Add again"), css_classes=["badge", "warning"], halign=Gtk.Align.CENTER)
        elif p.eyewear == "glasses":
            badge = Gtk.Label(label=_("With glasses"), css_classes=["badge"], halign=Gtk.Align.CENTER)
        else:
            badge = Gtk.Label(label=_("{n} positions").format(n=p.samples), css_classes=["caption", "dim-label"])
        box.append(badge)

        group = Gio.SimpleActionGroup()
        for name, cb in (("rename", self.ask_rename), ("remove", self.ask_delete)):
            act = Gio.SimpleAction.new(name, None)
            act.connect("activate", lambda *_a, cb=cb: cb(p))
            group.add_action(act)
        menu = Gio.Menu()
        menu.append(_("Rename…"), "face.rename")
        menu.append(_("Remove…"), "face.remove")
        card = Gtk.MenuButton(child=box, menu_model=menu, css_classes=["face-card"], tooltip_text=p.label)
        card.insert_action_group("face", group)
        if not p.usable:
            card.set_tooltip_text(_("Made with an older recognition model — add it again"))
        return card

    def _add_card(self):
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6, valign=Gtk.Align.CENTER)
        box.append(Gtk.Image(icon_name="list-add-symbolic", pixel_size=24, css_classes=["add-icon"]))
        box.append(Gtk.Label(label=_("Add a Face"), css_classes=["caption-heading"]))
        return Gtk.Button(child=box, css_classes=["face-card", "add-card"], action_name="win.add-face")

    def _toast_error(self, err):
        if error_code(err).endswith("AccessDenied"):
            self.toasts.add_toast(Adw.Toast(title=_("Authorization denied")))
        else:
            self.toasts.add_toast(Adw.Toast(title=error_text(err)))

    # -- tiles --------------------------------------------------------------

    def _on_tile(self, tile, key):
        if key in self.busy:
            return
        if key == "keyring":
            if tile.active:
                self._set_busy(key, True)
                self.daemon.clear_keyring_password(lambda _r, e: self._keyring_cleared(e))
            else:
                self._ask_keyring_password()
            return
        on = not tile.active
        self._set_busy(key, True)

        def done(_r, err):
            self._set_busy(key, False)
            if err:
                self._toast_error(err)
            self._load_status()

        self.daemon.set_pam_enabled(key.removeprefix("pam-"), on, done)

    def _set_busy(self, key, busy):
        (self.busy.add if busy else self.busy.discard)(key)
        self._update_status()

    def _keyring_cleared(self, err):
        self._set_busy("keyring", False)
        if err:
            self._toast_error(err)
        else:
            self.toasts.add_toast(Adw.Toast(title=_("Keyring will ask for your password again")))
        self._load_status()

    def _ask_keyring_password(self):
        d = Adw.AlertDialog(
            heading=_("Unlock Keyring With Your Face?"),
            body=_(
                "Your login password will be stored on this computer, encrypted by its TPM chip. "
                "It cannot be read from another computer or if the disk is removed, "
                "but an administrator of this running system could recover it.\n\n"
                "If you change your password or update the firmware, turn this option on again."
            ),
        )
        entry = Gtk.PasswordEntry(show_peek_icon=True, activates_default=True, placeholder_text=_("Login password"))
        d.set_extra_child(entry)
        d.add_response("cancel", _("Cancel"))
        d.add_response("ok", _("Turn On"))
        d.set_response_appearance("ok", Adw.ResponseAppearance.SUGGESTED)
        d.set_default_response("ok")
        d.set_close_response("cancel")

        def on_response(_d, r):
            password = entry.get_text()
            entry.set_text("")
            if r != "ok" or not password:
                return
            self._set_busy("keyring", True)
            self.daemon.set_keyring_password(password, self._keyring_set)

        d.connect("response", on_response)
        d.present(self)
        entry.grab_focus()

    def _keyring_set(self, _res, err):
        self._set_busy("keyring", False)
        if not err:
            self.toasts.add_toast(Adw.Toast(title=_("Your keyring will unlock when you log in with your face")))
        elif "wrong-password" in error_text(err):
            self.toasts.add_toast(Adw.Toast(title=_("Wrong password")))
        else:
            self._toast_error(err)
        self._load_status()

    # -- dialogs ------------------------------------------------------------

    def _entry_dialog(self, heading, body, text, ok_label, callback):
        d = Adw.AlertDialog(heading=heading, body=body)
        entry = Gtk.Entry(text=text, activates_default=True)
        d.set_extra_child(entry)
        d.add_response("cancel", _("Cancel"))
        d.add_response("ok", ok_label)
        d.set_response_appearance("ok", Adw.ResponseAppearance.SUGGESTED)
        d.set_default_response("ok")
        d.set_close_response("cancel")
        d.connect("response", lambda _d, r: r == "ok" and callback(entry.get_text().strip()))
        d.present(self)
        entry.grab_focus()

    def ask_label(self):
        if not self.daemon.proxy:
            return
        has_glasses = any(p.eyewear == "glasses" for p in self.profiles)
        suggestion = _("Without glasses") if has_glasses else (_("My face") if not self.profiles else "")
        self._entry_dialog(
            _("Add a Face"),
            _("Give it a name, for example “With glasses”. You'll then slowly turn your head while the camera captures several positions."),
            suggestion,
            _("Start"),
            self.start_enroll,
        )

    def start_enroll(self, label):
        page = EnrollPage(self.daemon, label or _("My face"))
        page.connect("finished", lambda *_: self._after_enroll(False))
        page.connect("test-requested", lambda *_: self._after_enroll(True))
        self.nav.push(page)
        page.start()

    def _after_enroll(self, test):
        self.nav.pop_to_tag("main")
        self.load()
        if test:
            self.open_test()

    def open_test(self):
        if not self.daemon.proxy or self.nav.get_visible_page().get_tag() != "main":
            return
        self.nav.push(TestPage(self.daemon, self.toasts))

    def ask_rename(self, p):
        def done(label):
            if label:
                self.daemon.rename_profile(p.id, label, lambda _r, e: self._toast_error(e) if e else self.load())

        self._entry_dialog(_("Rename Face"), "", p.label, _("Rename"), done)

    def ask_delete(self, p):
        d = Adw.AlertDialog(
            heading=_("Remove “{label}”?").format(label=p.label),
            body=_("You won't be able to unlock with this face anymore."),
        )
        d.add_response("cancel", _("Cancel"))
        d.add_response("delete", _("Remove"))
        d.set_response_appearance("delete", Adw.ResponseAppearance.DESTRUCTIVE)
        d.set_close_response("cancel")

        def on_response(_d, r):
            if r == "delete":
                self.daemon.delete_profile(p.id, lambda _r, e: self._toast_error(e) if e else self._deleted(p))

        d.connect("response", on_response)
        d.present(self)

    def _deleted(self, p):
        self.toasts.add_toast(Adw.Toast(title=_("“{label}” removed").format(label=p.label)))
        self.load()

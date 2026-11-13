import gettext
import locale
import os
import sys

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")

from gi.repository import Adw, Gdk, Gio, Gtk  # noqa: E402

APP_ID = "org.facelock.Facelock"
VERSION = "0.1.0"
DOMAIN = "facelock-gtk"



def setup_i18n():
    localedir = os.environ.get("FACELOCK_LOCALEDIR", "/usr/share/locale")
    try:
        locale.setlocale(locale.LC_ALL, "")
    except locale.Error:
        pass
    gettext.bindtextdomain(DOMAIN, localedir)
    gettext.textdomain(DOMAIN)


class App(Adw.Application):
    def __init__(self):
        super().__init__(application_id=APP_ID, flags=Gio.ApplicationFlags.DEFAULT_FLAGS)
        about = Gio.SimpleAction.new("about", None)
        about.connect("activate", self._about)
        self.add_action(about)
        self.set_accels_for_action("window.close", ["<Control>w"])
        self.set_accels_for_action("app.quit", ["<Control>q"])
        quit_ = Gio.SimpleAction.new("quit", None)
        quit_.connect("activate", lambda *_: self.quit())
        self.add_action(quit_)

    def do_startup(self):
        Adw.Application.do_startup(self)
        display = Gdk.Display.get_default()
        css = Gtk.CssProvider()
        from .style import CSS

        css.load_from_string(CSS)
        # Above USER so a ~/.config/gtk-4.0 theme doesn't flatten our few custom
        # classes (it only targets them).
        Gtk.StyleContext.add_provider_for_display(display, css, Gtk.STYLE_PROVIDER_PRIORITY_USER + 1)
        # Running from the source tree: the icon is not installed yet.
        icons = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "data", "icons")
        if os.path.isdir(icons):
            Gtk.IconTheme.get_for_display(display).add_search_path(icons)

    def do_activate(self):
        from .window import Window

        win = self.get_active_window() or Window(self)
        win.present()

    def _about(self, *_):
        from gettext import gettext as _

        Adw.AboutDialog(
            application_name=_("Face Unlock"),
            application_icon=APP_ID,
            version=VERSION,
            comments=_("Infrared face authentication with liveness detection"),
            license_type=Gtk.License.GPL_3_0,
        ).present(self.get_active_window())


def main():
    setup_i18n()
    return App().run(sys.argv)

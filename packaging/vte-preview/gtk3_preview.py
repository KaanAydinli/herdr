"""Full-color Herdr previews for a GTK 3 Vte.Terminal (VTE >= 0.78)."""
import ctypes
import os
import re
import stat
import struct
import tempfile

import gi

gi.require_version("Gtk", "3.0")
gi.require_version("GdkPixbuf", "2.0")
gi.require_version("Vte", "2.91")
from gi.repository import GdkPixbuf, GLib, Gtk, Vte

PROPERTY = "vte.ext.herdr.preview"
Vte.install_termprop(PROPERTY, Vte.PropertyType.STRING, Vte.PropertyFlags(0))


# The distro ships PyGObject without its optional cairo converter. Call the
# public GTK functions directly so the image surface retains its device scale.
_pointer = ctypes.pythonapi.PyCapsule_GetPointer
_pointer.argtypes = (ctypes.py_object, ctypes.c_char_p)
_pointer.restype = ctypes.c_void_p
_make_surface = ctypes.CDLL("libgdk-3.so.0").gdk_cairo_surface_create_from_pixbuf
_make_surface.argtypes = (ctypes.c_void_p, ctypes.c_int, ctypes.c_void_p)
_make_surface.restype = ctypes.c_void_p
_set_surface = ctypes.CDLL("libgtk-3.so.0").gtk_image_set_from_surface
_set_surface.argtypes = (ctypes.c_void_p, ctypes.c_void_p)
_set_surface.restype = None
_release = ctypes.CDLL("libcairo.so.2").cairo_surface_destroy
_release.argtypes = (ctypes.c_void_p,)
_release.restype = None


def load_frame(name):
    if not name or not re.fullmatch(r"herdr-preview-[A-Za-z0-9]{16}", name):
        return None
    path = os.path.join(GLib.get_user_runtime_dir(), name)
    fd = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as stream:
        st = os.fstat(fd)
        if not stat.S_ISREG(st.st_mode) or st.st_uid != os.getuid() or st.st_mode & 0o077:
            return None
        magic, x, y, w, h = struct.unpack("<4s4I", stream.read(20))
        if max(x, y) > 2**24:
            return None
        cw = ch = 0
        if magic == b"HDP2":
            cw, ch = struct.unpack("<2I", stream.read(8))
            if not (0 < cw <= 256 and 0 < ch <= 512):
                return None
        elif magic != b"HDP1":
            return None
        if not 0 < w*h <= 8*1024*1024 or st.st_size != stream.tell() + w*h*4:
            return None
        pixels = stream.read(w*h*4)
        if len(pixels) != w*h*4:
            return None
        pixbuf = GdkPixbuf.Pixbuf.new_from_bytes(
            GLib.Bytes.new(pixels), GdkPixbuf.Colorspace.RGB, True, 8, w, h, w*4)
        return x, y, cw, ch, pixbuf


class Preview:
    def __init__(self, terminal):
        self.terminal = terminal
        self.widget = Gtk.Overlay()
        self.widget.add(terminal)
        self.image = Gtk.Image()
        self.image.set_halign(Gtk.Align.START)
        self.image.set_valign(Gtk.Align.START)
        self.widget.add_overlay(self.image)
        self.widget.set_overlay_pass_through(self.image, True)
        self.pending = 0
        self.pitch = None
        try:
            self.metrics = tempfile.NamedTemporaryFile(prefix="herdr-cell-", dir=GLib.get_user_runtime_dir())
        except OSError:
            self.metrics = None
        for signal in ("char-size-changed", "notify::scale-factor", "realize"):
            terminal.connect(signal, self.update_metrics)
        self.update_metrics()
        terminal.connect("termprop-changed::" + PROPERTY, self.changed)
        terminal.connect("child-exited", lambda *_: terminal.reset_termprop(PROPERTY))
        terminal.connect("destroy", self.destroy)

    def environment(self):
        env = ["HERDR_VTE_PREVIEW=1"]
        if self.metrics is not None:
            env.append("HERDR_VTE_METRICS=" + os.path.basename(self.metrics.name))
        return env

    def update_metrics(self, *_):
        if self.metrics is None or self.metrics.closed:
            return
        scale = self.terminal.get_scale_factor()
        pitch = (max(1, min(256, self.terminal.get_char_width() * scale)),
                 max(1, min(512, self.terminal.get_char_height() * scale)))
        if self.pitch != pitch:
            try:
                if os.pwrite(self.metrics.fileno(), struct.pack("<2H", *pitch), 0) != 4:
                    return
            except OSError:
                return
            self.pitch = pitch
            self.changed()

    def destroy(self, *_):
        if self.metrics is not None:
            self.metrics.close()
        if self.pending:
            GLib.source_remove(self.pending)
            self.pending = 0

    def changed(self, *_):
        if not self.pending:
            self.pending = GLib.idle_add(self.refresh)

    def refresh(self):
        self.pending = 0
        name, _size = self.terminal.get_termprop_string(PROPERTY)
        try:
            frame = load_frame(name)
        except (OSError, ValueError, struct.error, GLib.Error):
            frame = None
        if frame:
            x, y, cw, ch, pixbuf = frame
            scale = self.terminal.get_scale_factor()
            sx = self.terminal.get_char_width() / cw if cw else 1
            sy = self.terminal.get_char_height() / ch if ch else 1
            padding = self.terminal.get_style_context().get_padding(self.terminal.get_state_flags())
            self.image.set_margin_start(round(x * sx) + padding.left)
            self.image.set_margin_top(round(y * sy) + padding.top)
            if cw:
                w = max(1, round(pixbuf.get_width() * sx * scale))
                h = max(1, round(pixbuf.get_height() * sy * scale))
                if (w, h) != (pixbuf.get_width(), pixbuf.get_height()):
                    if w * h > 8 * 1024 * 1024:
                        self.image.clear()
                        return GLib.SOURCE_REMOVE
                    pixbuf = pixbuf.scale_simple(w, h, GdkPixbuf.InterpType.BILINEAR)
                surface = _make_surface(_pointer(pixbuf.__gpointer__, None), scale, None)
                if surface:
                    try:
                        _set_surface(_pointer(self.image.__gpointer__, None), surface)
                    finally:
                        _release(surface)
            else:
                self.image.set_from_pixbuf(pixbuf)
        else:
            self.image.clear()
        return GLib.SOURCE_REMOVE

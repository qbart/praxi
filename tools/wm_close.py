"""Sends WM_DELETE_WINDOW to an X window, as a window manager's close button does.
Usage: wm_close.py WINDOW_ID (DISPLAY from the environment)."""
import ctypes, sys

x = ctypes.cdll.LoadLibrary("libX11.so.6")
x.XOpenDisplay.restype = ctypes.c_void_p
x.XOpenDisplay.argtypes = [ctypes.c_char_p]
x.XInternAtom.restype = ctypes.c_ulong
x.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
x.XSendEvent.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_long, ctypes.c_void_p]
x.XFlush.argtypes = [ctypes.c_void_p]
x.XCloseDisplay.argtypes = [ctypes.c_void_p]

class ClientMessage(ctypes.Structure):
    _fields_ = [("type", ctypes.c_int), ("serial", ctypes.c_ulong), ("send_event", ctypes.c_int),
                ("display", ctypes.c_void_p), ("window", ctypes.c_ulong), ("message_type", ctypes.c_ulong),
                ("format", ctypes.c_int), ("data", ctypes.c_long * 5)]

class XEvent(ctypes.Union):
    _fields_ = [("xclient", ClientMessage), ("pad", ctypes.c_long * 24)]

window = int(sys.argv[1], 0)
d = x.XOpenDisplay(None)
assert d, "cannot open display"
ev = XEvent()
ev.xclient.type = 33  # ClientMessage
ev.xclient.window = window
ev.xclient.message_type = x.XInternAtom(d, b"WM_PROTOCOLS", 0)
ev.xclient.format = 32
ev.xclient.data[0] = x.XInternAtom(d, b"WM_DELETE_WINDOW", 0)
ev.xclient.data[1] = 0  # CurrentTime
assert x.XSendEvent(d, window, 0, 0, ctypes.byref(ev)), "XSendEvent failed"
x.XFlush(d)
x.XCloseDisplay(d)

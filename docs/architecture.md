# Architecture

Go standard library only; the X11 host in five files, the Windows host
in five more, the macOS host in three, and the page:

- `x11.go`: an X11 client over the local socket. Connection setup with the
  MIT-MAGIC-COOKIE-1 from the Xauthority file, GetImage of the root window
  (ZPixmap, 32 bits per pixel), the keyboard mapping (GetKeyboardMapping),
  and XTEST FakeInput for pointer moves, buttons (4/5 = wheel) and keys.
- `clipboard.go`: the X11 clipboard on a second connection, driven by one
  goroutine. It owns the CLIPBOARD selection from an unmapped input-only
  window to offer the viewer's text (SelectionRequest answered for
  TARGETS, UTF8_STRING, TEXT and STRING, the property written in pieces
  the server accepts), and reads another program's text with
  ConvertSelection and GetProperty, INCR included. Text only, at most 1 MB
  either way; more is refused with a message.
- `rfb.go`: a VNC (RFB 3.8) client for a VNC server on loopback, as a
  screen: VNC authentication (security type 2, DES), a 32-bit BGRX pixel
  format like X11's, Raw and CopyRect updates and desktop size changes
  kept in a framebuffer, PointerEvent (buttons as a mask, 4/5 = wheel) and
  KeyEvent (X keysyms, as keys.go makes them). A capture asks for what
  changed and waits for it, at most a second. A dropped connection is
  opened again on the next call, with the same password. Anything the
  server sends that was not asked for ends the connection with its name.
- `pasteboard.go`: a clipboard through two commands; on a Mac `pbpaste`
  and `pbcopy`, with `LANG=en_US.UTF-8` (without it they speak MacRoman
  and lose what it lacks). Text only, at most 1 MB either way.
- `host_x11.go` (Linux): the X11 screen and clipboard. `host_darwin.go`
  (macOS): macOS's own Screen Sharing on `127.0.0.1:5900` through `rfb.go`,
  with the VNC password from `~/.config/rustopviewer/vnc-password` (refused
  when others can read it), and the pasteboard; `rustopviewer install`
  makes the password and waits until Screen Sharing takes it.

  Why Screen Sharing on a Mac (measured on macOS 26.6.2): an SSH session
  cannot capture the screen (`screencapture` says "could not create image
  from rect"), so a program of our own would have to run in the signed-in
  session with macOS's Screen Recording and Accessibility permissions,
  which macOS keeps per program path (a new path with every versioned
  install) and asks for again, and would need cgo or hand-written assembly
  to reach CoreGraphics. Screen Sharing is Apple's, holds those
  permissions, works at the lock screen and starts only on a connection;
  RustOp Viewer stays a cross-built Go binary that asks macOS for nothing.
- `keys.go`: browser key names to X keysyms (X11 and RFB both use them).
- `server.go`: HTTP. `/?k=KEY` sets the session cookie; `/`,
  `/viewer.js` and `/viewer.css` are the page; `/stream` is multipart
  JPEG; `/input` takes a JSON batch of events; `/clipboard/set` and
  `/clipboard/get` take and give clipboard text as JSON POSTs; `/info`
  gives the screen size and whether the clipboard works. A capture loop
  runs only while someone watches and publishes a frame only when the
  picture changed (CRC-32 of the pixels), at most 8 per second.
- `main.go`: `serve`, listening on a private socket or loopback only;
  `install` and `uninstall` for what a platform needs once; `host_x11.go`
  opens the X11 host, `host_windows.go` the Windows one, `host_darwin.go`
  the macOS one.
- `screen_windows.go`: the primary monitor read with GDI (BitBlt into a
  bitmap, GetDIBits out of it, CAPTUREBLT for layered windows), per-monitor
  DPI awareness so a scaled display is read in real pixels, SendInput for
  the mouse (absolute coordinates over the primary monitor, the wheel in
  notches) and the keyboard.
- `winkeys.go`: how a Windows host types a key, kept apart so it is
  tested anywhere: named keys as virtual keys; a character as the key that
  types it on the focused window's layout (VkKeyScanEx), with Shift for
  the press if needed, so Ctrl+C or Win+R work; otherwise (not on the
  layout, needs AltGr, or Shift held where the layout needs none) as the
  character itself (KEYEVENTF_UNICODE). Every press gets its release.
- `clipboard_windows.go`: CF_UNICODETEXT through OpenClipboard,
  GlobalAlloc and SetClipboardData, read back with GetClipboardData;
  CRLF there, LF here; at most 1 MB of UTF-8 either way.
- `host_windows.go`, `winhost.go`: over SSH (a window station that is not
  interactive, as in session 0) `serve` writes the session's key and
  address to `%LOCALAPPDATA%\rustopviewer\session`, opened with
  FILE_FLAG_DELETE_ON_CLOSE so Windows removes it when `serve` exits, and
  runs the user's task (`schtasks /run`). The task runs a copy of this
  program marked as a GUI program (PE subsystem 2, what `-H windowsgui`
  sets, so no console window opens) as `rustopviewer host`: it reads the
  file, serves on the loopback address, writes `ready ADDR` or `error: ...`
  to `host.log` for `serve` to read, and stops when the file goes. The
  task (`schtasks /create /xml`): InteractiveToken, LeastPrivilege, no
  trigger, on battery too, no time limit, one at a time.
- `viewer.html`, `viewer.js`, `viewer.css`: the page. Ctrl+V lets the
  browser's paste event hand over its clipboard (no permission needed);
  input waits until the host owns that text, then the keys go through.
  After a copy shortcut it fetches the host's text and writes it with
  `navigator.clipboard.writeText()` when it is new. `/info` says the
  host's system; between a Mac and a computer that is not one, Control and
  Meta (⌘) change places in the keys it sends, and the paste it presses is
  ⌘V on a Mac.

## With LinuxMice

`lmice desktop DEVICE` asks the device for its home and viewer version,
opens one SSH session through the fleet's pinned keys (and jump, if any)
that forwards a free local port to the device's socket and runs
`rustopviewer serve --until-stdin-closes`, reads the `ready key=` line,
opens the browser, and ends the session (and with it the viewer) when you
press Enter.

# Architecture

Five Go files and the page, Go standard library only:

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
- `keys.go`: browser key names to X keysyms.
- `server.go`: HTTP. `/?k=KEY` sets the session cookie; `/`,
  `/viewer.js` and `/viewer.css` are the page; `/stream` is multipart
  JPEG; `/input` takes a JSON batch of events; `/clipboard/set` and
  `/clipboard/get` take and give clipboard text as JSON POSTs; `/info`
  gives the screen size and whether the clipboard works. A capture loop
  runs only while someone watches and publishes a frame only when the
  picture changed (CRC-32 of the pixels), at most 8 per second.
- `main.go`: `serve`, listening on a private socket or loopback only.
- `viewer.html`, `viewer.js`, `viewer.css`: the page. Ctrl+V lets the
  browser's paste event hand over its clipboard (no permission needed);
  input waits until the host owns that text, then the keys go through.
  After a copy shortcut it fetches the host's text and writes it with
  `navigator.clipboard.writeText()` when it is new.

## With LinuxMice

`lmice desktop DEVICE` asks the device for its home and viewer version,
opens one SSH session through the fleet's pinned keys (and jump, if any)
that forwards a free local port to the device's socket and runs
`rustopviewer serve --until-stdin-closes`, reads the `ready key=` line,
opens the browser, and ends the session (and with it the viewer) when you
press Enter.

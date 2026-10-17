# Architecture

Four files, Go standard library only:

- `x11.go`: an X11 client over the local socket. Connection setup with the
  MIT-MAGIC-COOKIE-1 from the Xauthority file, GetImage of the root window
  (ZPixmap, 32 bits per pixel), the keyboard mapping (GetKeyboardMapping),
  and XTEST FakeInput for pointer moves, buttons (4/5 = wheel) and keys.
- `keys.go`: browser key names to X keysyms.
- `server.go`: HTTP. `/?k=KEY` sets the session cookie; `/` and
  `/viewer.js` are the page; `/stream` is multipart JPEG; `/input` takes a
  JSON batch of events; `/info` gives the screen size. A capture loop runs
  only while someone watches and publishes a frame only when the picture
  changed (CRC-32 of the pixels), at most 8 per second.
- `main.go`: `serve`, listening on a private socket or loopback only.

## With LinuxMice

`lmice desktop DEVICE` asks the device for its home and viewer version,
opens one SSH session through the fleet's pinned keys (and jump, if any)
that forwards a free local port to the device's socket and runs
`rustopviewer serve --until-stdin-closes`, reads the `ready key=` line,
opens the browser, and ends the session (and with it the viewer) when you
press Enter.

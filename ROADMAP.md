# Roadmap

1. **Linux X11 hosts** (done in 0.3): screen, mouse, keyboard, browser viewer.
2. **Windows hosts** (done in 0.4): capture, input and the clipboard
   through Windows' own DLLs, reached over Windows' OpenSSH server; the
   screen shown by a per-user Task Scheduler task only while a session
   lasts. Next: other monitors than the primary, and DXGI Desktop
   Duplication if GDI proves too slow on a real 4K screen.
3. **macOS hosts** (done in 0.4): through macOS's own Screen Sharing,
   the clipboard with pbcopy and pbpaste.
4. **Choosing a monitor** on multi-monitor desktops, and lower bandwidth
   (sending only changed areas).
5. **Clipboard** in both directions: text on Linux X11, Windows and macOS
   hosts is done (0.4, up to 1 MB), and Control and ⌘ change places
   between a Mac and a computer that is not one. Next: images and files.
6. **Phones**: sign-in through an OpenID Connect provider for viewers that
   cannot open an SSH tunnel.
7. **Wayland** hosts, through the desktop portal.

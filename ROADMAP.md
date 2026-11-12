# Roadmap

1. **Linux X11 hosts** (done in 0.3): screen, mouse, keyboard, browser viewer.
2. **Windows hosts** (done, unreleased): capture, input and the clipboard
   through Windows' own DLLs, reached over Windows' OpenSSH server; the
   screen shown by a per-user Task Scheduler task only while a session
   lasts. Next: other monitors than the primary, and DXGI Desktop
   Duplication if GDI proves too slow on a real 4K screen.
3. **Choosing a monitor** on multi-monitor desktops, and lower bandwidth
   (sending only changed areas).
4. **Clipboard** in both directions: text on Linux X11 hosts is done
   (unreleased, up to 1 MB), and on Windows hosts. Next: macOS hosts,
   images and files, and ⌘ sent as Ctrl from Mac viewers.
5. **Phones**: sign-in through an OpenID Connect provider for viewers that
   cannot open an SSH tunnel.
6. **Wayland** hosts, through the desktop portal.

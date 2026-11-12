# Roadmap

1. **Linux X11 hosts** (done in 0.3): screen, mouse, keyboard, browser viewer.
2. **Windows hosts**: capture and input through Windows' own DLLs (no
   third-party code), reached through a WSL distribution on the same
   machine so Windows itself opens no port.
3. **macOS hosts** (done, unreleased): through macOS's own Screen Sharing,
   the clipboard with pbcopy and pbpaste.
4. **Choosing a monitor** on multi-monitor desktops, and lower bandwidth
   (sending only changed areas).
5. **Clipboard** in both directions: text on Linux X11 and macOS hosts is
   done (unreleased, up to 1 MB), and Control and ⌘ change places between
   a Mac and a computer that is not one. Next: the clipboard on Windows
   hosts, images and files.
6. **Phones**: sign-in through an OpenID Connect provider for viewers that
   cannot open an SSH tunnel.
7. **Wayland** hosts, through the desktop portal.

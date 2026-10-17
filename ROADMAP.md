# Roadmap

1. **Linux X11 hosts** (done in 0.3): screen, mouse, keyboard, browser viewer.
2. **Windows hosts**: capture and input through Windows' own DLLs (no
   third-party code), reached through a WSL distribution on the same
   machine so Windows itself opens no port.
3. **Choosing a monitor** on multi-monitor desktops, and lower bandwidth
   (sending only changed areas).
4. **Clipboard** in both directions.
5. **Phones**: sign-in through an OpenID Connect provider for viewers that
   cannot open an SSH tunnel.
6. **Wayland** hosts, through the desktop portal.

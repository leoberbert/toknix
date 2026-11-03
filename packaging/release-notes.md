## Linux

Download `toknix-<version>-linux-x86_64.AppImage`, mark it executable, and run it. Rust is not required.

The AppImage needs glibc from Ubuntu 24.04 or newer. Current Fedora releases qualify. If it does not start, run it with `--appimage-extract-and-run`.

## Windows

Download `toknix-<version>-windows-x86_64.zip`, extract it, and run `toknix.exe`. Rust is not required. Opening it from Explorer does not leave a command window open. From a terminal, Ctrl+C stops the server.

## Both

Toknix opens a browser page on this computer, at `http://127.0.0.1:4780` when that port is free. It reads the sessions and keys already on the machine. Nothing is sent anywhere except to each provider's own usage endpoint.

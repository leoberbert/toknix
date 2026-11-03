# Distribution

People can run Toknix from a GitHub release without installing Rust. In Actions, open the Release workflow and choose Run workflow. An empty version uses `version` in `Cargo.toml`. The workflow checks that the requested version matches the crate, then publishes two packages:

- `toknix-X.Y.Z-linux-x86_64.AppImage`, built on Ubuntu 24.04. It needs that glibc or newer, which current Fedora releases provide. Mark it executable and run it. `--appimage-extract-and-run` avoids a FUSE dependency.
- `toknix-X.Y.Z-windows-x86_64.zip`, containing `toknix.exe`.

Pushing a tag `vX.Y.Z` that matches `Cargo.toml` publishes the same packages.

## Windows console

The Windows binary uses the GUI subsystem, the same choice as ClipForge. Opening `toknix.exe` from Explorer starts the server and the browser and does not leave a command window open. Running it again reopens the page if the server is already up.

When the program is started from PowerShell or Command Prompt, it attaches to that console, prints the address, and stops on Ctrl+C.

## What is not bundled

The AppImage and the zip contain the Toknix program only. Browser cookies, API keys, and the system secret store stay on the machine that runs it.

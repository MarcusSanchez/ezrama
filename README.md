# ezrama

Keeps a TRYX Panorama SE cooler display showing its stored media on Windows
without the KANALI application running. One small process, one keepalive
every 4.5 seconds, no sensor reads, no dependencies. KANALI stays the tool
for choosing media; ezrama shows whatever it last stored on the panel.

## Install

With a stable Rust toolchain:

```
cargo build --release
target\release\ezrama.exe install
```

This copies the binaries to `%LOCALAPPDATA%\ezrama`, adds a logon entry for
the windowless watcher, and starts it. Turn off "start with Windows" in
KANALI's own settings, or the two race for the panel at logon. `ezrama uninstall`
reverses it.

The watcher shows a tray icon with the current status and a menu: Pause,
Resume, Open KANALI, Quit.

## Running alongside KANALI

Both can talk to the panel at once, and viewing works, but uploads stall
partway and the panel stays stuck until KANALI is closed and reopened. To
change media, use **Open KANALI** from the tray icon or `ezrama kanali`:
ezrama releases the panel, starts KANALI, and takes the panel back when
KANALI exits. If you start KANALI some other way, `ezrama pause` first and
`ezrama resume` after; ezrama only knows about the copy it started itself.

## Commands

```
ezrama probe      Find the Panorama SE printer interface and open it briefly
ezrama info       Start a session and print the device's state; changes nothing
ezrama activate   Start a session and switch the panel to its stored media once
ezrama run        Start a session and hold it with keepalive pings until Ctrl+C
ezrama watch      Hold a session whenever the panel is present, with a tray icon
ezrama pause      Ask the running watcher to release the panel and wait for it
ezrama resume     Ask the running watcher to take the panel back
ezrama kanali     Ask the running watcher to release the panel, start KANALI,
                  and take the panel back once KANALI exits
ezrama stop       Ask the running watcher to exit
ezrama install    Copy the binaries to local app data, start the watcher at logon
ezrama uninstall  Stop the watcher and remove the logon entry and the binaries
ezrama status     Report the installation, the watcher, and the panel
```

`-v` adds detail, including one log line per ping for `run` and `watch`.
`--interval <secs>` changes the ping interval for `run` and `watch`; the
panel blanks after five seconds of silence, so stay under that.

`ezrama.exe` is the console binary. `ezramaw.exe` is the same program built
without a console, which is what the logon entry runs. The watcher logs to
`%LOCALAPPDATA%\ezrama\ezrama.log`.

## Footprint

One process, under a megabyte resident and a few megabytes committed,
idle except for one small USB write every 4.5 seconds, waking once per
write. Device arrival, removal, sleep, and resume are handled through
Windows notifications, not polling.

## Thanks

The wire protocol was worked out with the help of
[Tryx-Linux-GUI](https://github.com/DXVSI/Tryx-Linux-GUI), which keeps the
same panels alive on Linux. Smol shout-out; it saved a lot of guessing.

## License

MIT. See `LICENSE`.

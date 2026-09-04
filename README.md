# ezrama

Keeps a TRYX Panorama cooler display showing its stored media on Windows
without the KANALI application running. One small process, one keepalive
every 4.5 seconds, no sensor reads, no dependencies. KANALI stays the tool
for choosing media; ezrama shows whatever it last stored on the panel.

## Supported displays

| Display | USB id | Status |
|---|---|---|
| Panorama SE (ARGB 240 / 360) | `391a:1021` | Tested on real hardware |
| Panorama (240 / 280 / 360, ARGB) | `391a:1011` | Expected to work, untested: the reference runtime drives it with the same session start and keepalive |
| Turris 620 | `391a:2011` | Not supported: the reference runtime sends it no keepalive, so there is nothing known for ezrama to do |

If you run a Panorama and it works, or does not, an issue with the log
from `%LOCALAPPDATA%\ezrama\ezrama.log` is the fastest way to move it into
the tested row.

## Install

Download `ezrama.exe` from the releases page and double-click it. Windows
may warn that the program is unsigned; choose "More info" and "Run
anyway". A window lists what was done and waits for Enter.

That one step copies the program to `%LOCALAPPDATA%\ezrama`, starts it, and
adds four things: a logon entry so it starts with Windows, a Start Menu
entry named ezrama for getting it back after a Quit, an entry under
Settings > Apps whose Uninstall removes everything again, and the folder on
your PATH so `ezrama` works by name in any new terminal. Turn off "start
with Windows" in KANALI's own settings, or the two race for the panel at
logon.

While it runs, ezrama sits in the notification area. Its menu shows the
current status and offers Pause, Resume, Open KANALI, a Start with Windows
toggle, and Quit.

To build it yourself instead, use a stable Rust toolchain:

```
cargo build --release
target\release\ezrama.exe install
```

## Running alongside KANALI

KANALI and ezrama can run at the same time. Be cautious with uploads: a
media upload can stall while ezrama is also talking to the panel. Pause
ezrama from the tray before uploading and resume it after. Open KANALI
from the tray does that for you and takes the panel back when KANALI
closes. ezrama does not watch for KANALI on its own. That is deliberate: a
process watcher is the kind of background polling ezrama exists to avoid.

## Commands

```
ezrama probe      Find the display's printer interface and open it briefly
ezrama info       Start a session and print the device's state; changes nothing
ezrama activate   Start a session and switch the panel to its stored media once
ezrama run        Start a session and hold it with keepalive pings until Ctrl+C
ezrama watch      Hold a session whenever the panel is present, with a tray icon
ezrama pause      Ask the running watcher to release the panel and wait for it
ezrama resume     Ask the running watcher to take the panel back
ezrama kanali     Ask the running watcher to release the panel, start KANALI,
                  and take the panel back once KANALI exits
ezrama stop       Ask the running watcher to exit
ezrama install    Copy the program to local app data, add the logon, Start
                  Menu, Settings, and PATH entries, and start the watcher
ezrama uninstall  Stop the watcher and remove everything install added
ezrama status     Report the installation, the watcher, and the panel
```

`-v` adds detail, including one log line per ping for `run` and `watch`.
`--interval <secs>` changes the ping interval for `run` and `watch`; the
panel blanks after five seconds of silence, so stay under that.

`install` writes a second copy, `ezramaw.exe`, which is the same program
marked to run without a console; the logon and Start Menu entries run
that one. The watcher logs to `%LOCALAPPDATA%\ezrama\ezrama.log`, which
uninstall keeps.

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

# pCloudCC GUI

A lightweight Rust desktop app for controlling the
[pcloudcc-lneely](https://github.com/Mathew-D/pcloudcc-lneely) command-line client.
It uses the existing `pcloudcc` process and command interface; it does not
reimplement the sync engine.

## Features

- Overview with connection state, mount point, cache limit, and daemon details
- Start and stop controls, including confirmation before stopping with pending transfers
- Closing the window keeps the GUI and daemon running in the system tray
- Exit stops the daemon, including one started outside the GUI
- Minimize to the system tray and restore the window from the tray icon
- Sync-folder listing and pause/resume controls
- Crypto-folder unlock and lock controls
- Settings for the account email, mount point, cache, logging, and executable path
- Settings are stored in the user's configuration directory; account and crypto
  passwords are not stored by the GUI
- Process commands run in a background worker so the window remains responsive

## Requirements

- Rust 1.95 or later
- A built `pcloudcc` binary from pcloudcc-lneely, installed separately
- Linux with an X11 or Wayland display server
- A session D-Bus and a desktop system tray supporting StatusNotifierItem

## Build and run

Build and install pcloudcc-lneely separately using its build instructions. The
GUI does not require the pending-count modification; its exit confirmation
uses the existing `finalize` command instead. Compatibility with other
pcloudcc implementations is not guaranteed.

```bash
cd pcloudcc-gui
cargo build --release
./target/release/pcloudcc-gui
```

The GUI looks for `pcloudcc` in the current directory, the parent directory,
the repository directory when run from a Cargo build, common system paths, and
`PATH`. If it is installed elsewhere, set the path in **Settings** and select
**Use this executable**. If a previously saved path is wrong, use **Find
automatically**, then **Use this executable**. A separate source checkout is
not automatically searched; select its `pcloudcc` binary explicitly or install
it in a directory on `PATH`.

After starting, the GUI waits up to 15 seconds for the daemon to respond and
shows a readable status, transfer details, and optional technical output.
Status refreshes automatically every three seconds while connecting or waiting
for authentication, and every 15 seconds when ready or syncing. A responding
daemon does not necessarily mean the account is authenticated or the
filesystem is mounted; the displayed mount point is only the configured value.
If the daemon does not respond, check the configured log file (by default
`~/.pcloud/debug.log`) and refresh the status before retrying.

Close the window or use **Minimize to tray** to hide it without stopping the daemon.
The window is recreated on restore, while the GUI state and background worker
remain in memory. This also works on native Wayland without Xwayland.
The tray icon is blue with a green check when the daemon reports an online
state, and grey with a red cross when disconnected or when the reported state
does not establish connectivity (including connecting, scanning, or paused).
It follows automatic status refreshes even while the window is hidden.
The window's minimize control may also send it to the tray, depending on the
desktop. Select the tray icon or **Show pCloudCC** in its
menu to restore it. **Exit** in the window and **Quit and stop pCloudCC** in
the tray menu use the same graceful shutdown and pending-transfer confirmation.
**Open pCloud folder** opens the mounted drive using `xdg-open`. It locates
the actual `pCloud.fs` FUSE mount, including one started outside the GUI. If
multiple pCloud drives are mounted, set the exact absolute mount point in
Settings to select one. Errors restore the window when it is hidden.
If the desktop does not provide a compatible tray, closing the window leaves
it open with a message; use **Exit** to stop the daemon and close the GUI.
The minimize-to-tray button is disabled and normal window minimization remains available. If the tray
disconnects while hidden, the GUI restores its window.

## Account passwords

Enter the account password on the Overview page when pcloudcc has not already
saved it. The password is passed to the pcloudcc process through its supported
`PCLOUD_ACCOUNT_PASSWORD` environment variable and is not written to GUI
settings. The **Ask pcloudcc to save the account password** option enables
pcloudcc's own password-saving behavior on the device.

The Overview page always shows password and 2FA fields. A code entered before
starting the daemon or sending a password is held in memory and sent once if
pCloud requests 2FA. If no challenge is requested, the code is discarded when
sign-in completes. If a challenge appears without a code, or a code is
rejected, enter a new code and press Enter. pCloud decides whether to request
2FA, and codes may expire while waiting. The GUI does not save them.
Acknowledgment that a credential was sent does not mean it was accepted.

Crypto passwords are sent only when unlocking the encrypted folder and are not
saved.

Exiting the GUI requests a graceful daemon shutdown, including if the daemon
was started elsewhere. If transfers are pending, the GUI asks whether to stop
anyway or keep running. If shutdown fails, the window stays open and displays
the error. A forced termination or system crash cannot guarantee shutdown.

## License

Same license as the [pCloudCC project](https://github.com/lneely/pcloudcc-lneely/).

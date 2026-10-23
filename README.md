# claude-ticker

A small e-paper display that shows your Claude Pro/Max plan usage: the 5-hour session limit, the weekly limit, and any model-specific weekly limit, each with percent used and reset time. It runs on a Raspberry Pi Zero W with a Waveshare 2.13" e-paper HAT.

![UI mockup](docs/ui-mockup.png)

Contributor and agent rules (architecture, domain rules, layout constraints) are in [AGENTS.md](AGENTS.md).

> The numbers come from the same **undocumented** endpoint Claude Code's `/usage` command reads. Anthropic can change it at any time. If the display shows "usage response has no session or weekly limit", the API has probably moved.

## Hardware

- Raspberry Pi Zero W (Core Electronics CE04754)
- Waveshare 2.13" e-paper HAT, 250×122, V4 (Core Electronics WS-12915)
- A 2×20 male header (the Zero W ships without one), a microSD card, and a 5.1 V micro-USB power supply

Full specs and prices are in [AGENTS.md §6](AGENTS.md#6-hardware).

## Sign-in

The Pi signs in to your Claude account once and then keeps itself signed in. Access tokens last a few hours; the Pi renews them automatically and saves the new ones in `/var/lib/claude-ticker/credentials.json`, which only the service can read.

When the Pi isn't signed in (first boot, or after the sign-in expires), the display shows **SIGN IN** with a QR code:

1. Scan it with your phone. The phone must be on the same Wi-Fi as the Pi.
2. On the page that opens, tap **Open Claude sign-in**, sign in and approve.
3. Copy the code Claude shows, come back to the first tab, paste it, and tap **Finish**.

The display switches to your usage within a few seconds. The page only exists while the QR code is on screen, at a random address that changes every time, and it accepts one sign-in. Set `sign_in_port = 0` in the config to turn it off.

Prefer a terminal? Over SSH, run:

```sh
sudo claude-ticker login
```

It prints the sign-in link instead and asks for the code in the terminal. Either way, the running display picks up the new sign-in at its next check; no restart needed.

The sign-in itself eventually expires too. When that is less than 5 days away the footer shows **re-login in 4d 23h** (counting down); run `sudo claude-ticker login` any time before then, or let it lapse and scan the QR code that appears. `claude-ticker --check` prints the exact expiry.

Both use the same paste-the-code sign-in flow as Claude Code on headless machines, and the same OAuth client. Like the usage endpoint, it is undocumented and could change.

### Alternative: copy your Mac's sign-in

If you'd rather not sign in on the Pi, you can export Claude Code's credentials from the Mac. Every refresh **replaces the refresh token**, so the Pi and the Mac must not share one sign-in or they'll keep logging each other out:

1. On the Mac: `security find-generic-password -s "Claude Code-credentials" -w > credentials.json && chmod 600 credentials.json`
2. Install it on the Pi: `sudo ./deploy/install.sh ./claude-ticker ./credentials.json`
3. Back on the Mac, run `/login` in Claude Code so the Mac gets a fresh sign-in of its own.
4. Delete the exported `credentials.json` on both machines (the installed copy lives on in `/var/lib/claude-ticker`). Treat it like a password.

## Configure

`/etc/claude-ticker/config.toml`. Every key is optional:

```toml
timezone = "Australia/Sydney"   # reset times are shown in this zone; omit to use the Pi's OS timezone

[display]
poll_seconds = 300              # fetch every 5 minutes (minimum 60)
stale_minutes = 15              # mark the numbers STALE after this long without an update
```

All options are listed in [AGENTS.md §4](AGENTS.md#4-configuration).

## Develop on a laptop (no Pi needed)

```sh
cargo test
cargo run --example preview --features preview -- 62 23 4 out/   # PNGs of every screen
```

To sign in and check against the real API from a laptop (this writes and later refreshes the file the config points at):

```sh
cargo run --features net -- login my-config.toml     # with [account] credentials = "path/to/credentials.json"
cargo run --features net -- --check my-config.toml
```

The live simulator opens a desktop window that behaves like the panel, with fake usage you control from the keyboard:
- ↑/↓ change the session %, and [/] change the week %.
- ←/→ move time by an hour.
- O toggles an Opus row.
- X simulates network failures.
- A signs out (showing the QR screen); press it again to play the phone completing the sign-in.
- L simulates an expiring sign-in.
- Esc quits.

```sh
brew install sdl2
export LIBRARY_PATH="$(brew --prefix)/lib:$LIBRARY_PATH"   # Apple Silicon
cargo run --example simulator --features simulator -- --poll-seconds 5
```

## Build and install on the Pi

1. Enable SPI by adding `dtparam=spi=on` to `/boot/firmware/config.txt`, then reboot.
2. Cross-compile on your laptop and copy the binary and the `deploy/` folder to the Pi. The Zero W is ARMv6, so use 32-bit Raspberry Pi OS and this target. `cross` needs Docker running.

   ```sh
   cargo install cross
   cross build --release --features epd --target arm-unknown-linux-gnueabihf
   scp -r target/arm-unknown-linux-gnueabihf/release/claude-ticker deploy pi@<host>.local:
   ```

3. On the Pi, install it as a service that starts at boot:

   ```sh
   sudo ./deploy/install.sh ./claude-ticker
   sudo nano /etc/claude-ticker/config.toml      # set your timezone
   sudo systemctl restart claude-ticker
   # then scan the SIGN IN QR code on the display (see "Sign-in" above)
   ```

The installer does the following:

- Copies the binary to `/usr/local/bin`.
- Installs [deploy/claude-ticker.service](deploy/claude-ticker.service).
- Creates `/etc/claude-ticker/config.toml` from the example. If the file already exists, it's left alone.
- Installs a credentials file into the service's private state directory, if you passed one (the Mac-copy alternative).
- Enables and starts the service.

You can re-run it to upgrade; it never touches your config or sign-in.

The service starts at every boot and restarts automatically if it crashes. It fetches usage every `poll_seconds` and updates the screen with a quick partial refresh. About once an hour, and at midnight, it does a full refresh (with a black/white flash) to clear ghosting. If the screen hasn't changed, nothing is redrawn.

The Pi has no real-time clock, so after each boot the app doesn't trust the time until the OS has set it from the network (NTP). Until then it shows **SETTING THE CLOCK**, and it switches to the dashboard by itself once the time is set.

If a fetch fails (for example, during a Wi-Fi drop), the last numbers stay on screen. After `stale_minutes` they're marked **STALE**. After six times that, they're replaced by an error message.

### Useful commands

```sh
systemctl status claude-ticker           # running?
journalctl -u claude-ticker -f           # logs
sudo systemctl restart claude-ticker     # apply config changes
sudo claude-ticker login                 # sign in again
claude-ticker --check                    # fetch once, print usage and sign-in expiry
sudo systemctl disable --now claude-ticker   # stop starting at boot
```

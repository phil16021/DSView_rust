# Recording DSLogic USB traffic (Phase 0 baseline)

These recordings capture what the **C** driver sends to and receives from a real DSLogic. Phase 3
replays them against the Rust driver, which must send the same commands and handle the same responses.

## One-time setup (needs root)

```sh
sudo apt install wireshark-common       # provides dumpcap; answer "yes" to non-root capture
sudo usermod -aG wireshark "$USER"      # then log out and in again
sudo modprobe usbmon                    # kernel USB capture (not persistent across reboots)
lsusb | grep 2a0e                       # note the bus number, e.g. "Bus 001" -> usbmon1
```

`dumpcap` needs read access to `/dev/usbmonN`. If it reports a permission error, either run
`sudo setfacl -m u:$USER:r /dev/usbmon1` (resets at reboot) or install a udev rule like
`SUBSYSTEM=="usbmon", GROUP="wireshark", MODE="0640"`.

## Recording

1. Build and start the C implementation with its control socket enabled:

   ```sh
   /usr/bin/cmake -S . -B build && /usr/bin/cmake --build build -j
   ./build/DSView --mcp
   ```

   To include the firmware/FPGA upload in a trace, start `dumpcap -i usbmon1 -w startup.pcapng`
   **before** DSView and stop it (Ctrl-C) once DSView shows the device.

2. Run the scenarios. Each one becomes its own pcapng file in `testdata/usb/dslogic/`:

   ```sh
   cargo xtask usb record --iface usbmon1          # --dry-run: run the scenarios without recording
   ```

The scenarios run at 3 sample rates (the first one at or above 1 MHz, the middle one and the highest
the device offers) × 16/8/4 channels, plus two "trigger never fires, then stop" cases. `manifest.json`
records each scenario's capture result next to its trace.

Not automated, record by hand with dumpcap running: unplug and replug during a capture, and the
internal test-counter mode (Device Options → Operation Mode → Internal Test).

## Notes

- Each trace is a few MB. Keep them in the separate test-data repository (see
  `docs/migration/PLAN.md`, Phase 0) and commit only `manifest.json` plus hashes here.
- The C driver leaves some control-request header bytes uninitialised (see the driver analysis in
  `PLAN.md`). The replay backend masks them, so traces from different runs can differ in those bytes.

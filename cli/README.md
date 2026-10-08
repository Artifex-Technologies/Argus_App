# qFlipper-cli
### A non-interactive text mode interface for qFlipper
This program is mostly meant for testing purposes, although it can also provide all of the qFlipper's features from the comfort of the terminal emulator.

## Running:
### Windows:
`<Program_files_directory>\qFlipper\qFlipper-cli.exe [args] [parameters]`
### MacOS:
`<Applications_directory>/qFlipper.app/Contents/MacOS/qFlipper-cli [args] [parameters]`
### Linux:
`<AppImage_directory>/qFlipper-x86_64-x.y.z.AppImage cli [args] [parameters]`

## Chymaera subsystem (headless, no device required)

The Chymaera operator-console features are reachable headlessly — no GUI or display
server needed, which makes them easy to run and verify under WSL, over SSH, or in CI:

```
qFlipper-cli chymaera <command> [args]
```

Commands:
* `selftest` - Verify the whole Chymaera subsystem end-to-end (datastore, exporters,
  Flipper-file parsers, and a Sarina control-API ping round-trip). Prints PASS/FAIL per
  check and exits non-zero on any failure. **Run this first to confirm a build works.**
* `serve [port] [token]` - Start the Sarina control API on `127.0.0.1` (default port
  44700; optional shared token). Streams the live event log to stdout. See
  [../docs/SARINA_API.md](../docs/SARINA_API.md) for the command protocol.
* `parse <file>` - Parse a Flipper file (`.nfc`/`.sub`/...) and print the result as JSON.
* `import <file>` - Parse a capture file and record it in the datastore.

Example:
```sh
qFlipper-cli chymaera selftest
qFlipper-cli chymaera serve 44700
# from another shell:
printf '{"cmd":"status"}\n' | nc 127.0.0.1 44700
```

## Command syntax:
Run without any arguments to perform a quick update/repair.
### Commands:
* `backup <target_dir>` - Backup Internal Memory contents.
* `restore <source_dir>` - Restore Internal Memory contents.
* `erase` - Erase Internal Memory contents (Factory reset).
* `wipe` - Wipe entire MCU Flash Memory (Not implemented yet).
* `firmware <firmware_file.dfu>` - Flash Core1 Firmware.
* `core2radio <firmware_file.bin>` - Flash Core2 Radio stack.
* `core2fus <firmware_file.bin> <0xaddress>` - Flash Core2 Firmware Update Service **(WARNING! It WILL invalidate your secure enclave!)**

### Options:
* `-d <n>, --debug-level <n>` - Set debug output level, 0 - errors only, 1 - terse, 2 - everything. Default is 1.
* `-n <n>, --repeat-number <n>` - Repeat an operation *n* times, 0 - indefinitely, default - once.
* `-c <channel>, --update-channel <channel>` - Set the update channel (may be one of: `release`, `release-candidate`, `development`). The choice is saved in the configuration file, default is `release`.
* `-v, --version` - Show program version.
* `-h, --help` - Show help.

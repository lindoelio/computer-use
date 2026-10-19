# SkyLight

`computer-use` can call private SkyLight symbols so a target process looks frontmost without `NSRunningApplication`. That path is off unless the binary is built with `--features skylight` and the caller passes `--tier synthetic` or `--synthetic-focus`.

These calls are private, version-skewed, and unsigned. They may break on any macOS update. A missing symbol returns `unsupported` and names the symbol. The process does not abort. A non-zero status is `execution_failed`.

The crate opens `/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight` with `RTLD_LAZY` and uses the first symbol that `dlsym` resolves:

1. `SLPSSetFrontProcessWithOptions`
2. `SLSSetFrontProcessWithOptions`, which also needs `SLSMainConnectionID`
3. `CPSSetFrontProcess`

The options argument is 0. The process serial number comes from `GetProcessForPID`. The call does not restore the previous frontmost app.

## Symbols on this Mac

Checked with a read-only `dlsym` on macOS 27.0.1 (build 26A434). The symbols were not called.

| Symbol | Result |
| --- | --- |
| `SLPSSetFrontProcessWithOptions` | present (`0x18d4f29f4`) |
| `SLSSetFrontProcessWithOptions` | absent |
| `CPSSetFrontProcess` | present (`0x18d4f29f8`) |
| `SLSMainConnectionID` | present (`0x18d61fca0`) |

The first symbol that resolves is `SLPSSetFrontProcessWithOptions`.

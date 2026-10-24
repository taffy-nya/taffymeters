# Changelog

All notable changes to this project are documented here.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.2.0] - 2026-10-24

A complete rewrite.

### Added

- Multiple windows: split any window into panels, detach a panel into its own window, move panels between windows.
- Windows snap to each other and to screen edges, move together as a group, and can stay on top or be locked in place.
- Settings window (right-click) with Module, Window, Workspace and Theme pages; every panel has its own settings, each resettable to its default.
- Loudness Meter (momentary LUFS). Level Meter shows per-channel sample peak with peak hold.
- Spectrum styles Curve, Bars and Pixel, with peak hold and a hover readout.
- Spectrogram gain; flow direction for Spectrogram and Waveform.
- A workspace reference level that scales all panels.
- Dark and Light themes (Catppuccin Mocha and Latte), with per-type and per-panel colors.
- The session is saved automatically and restored at startup; built-in Default, Bar - Top and Bar - Bottom presets; save your own presets.
- Choose the audio input: any playback device (loopback) or a microphone. A lost input falls back to the default device.
- Frame rate limit, 60 fps by default.
- `--config-dir <dir>` keeps settings in another folder.
- Startup errors are shown in a dialog; crashes leave a log in the settings folder.
- Launching again with the same settings folder brings the running instance to the front.

### Changed

- Analysis runs on a background thread; panels with identical settings share the work, and windows redraw only when new audio arrives.
- Right-click opens the settings window instead of an in-panel menu.
- `Esc` no longer closes the window (`Ctrl+W` still does).
- The release package includes third-party licenses.
- Building requires Rust 1.92.

## [0.1.3] - 2026-04-19

### Added

- Flow direction for Spectrogram and Waveform.

### Changed

- Faster real-time updates of Spectrogram and Waveform.
- Low-power DX12 rendering.

### Removed

- Transparent window.

## [0.1.2] - 2026-04-03

### Changed

- Upgraded eframe to 0.34.1, which significantly reduces CPU usage at the cost of somewhat higher memory usage.

## [0.1.1] - 2026-04-02

### Added

- Waveform and Level Meter.

## [0.1.0] - 2026-03-29

Initial release: oscilloscope, spectrum analyzer, spectrogram and stereometer in a frameless, transparent, always-on-top window whose panels can be split and resized.

[Unreleased]: https://github.com/taffy-nya/taffymeters/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/taffy-nya/taffymeters/compare/v0.1.3...v0.2.0
[0.1.3]: https://github.com/taffy-nya/taffymeters/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/taffy-nya/taffymeters/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/taffy-nya/taffymeters/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/taffy-nya/taffymeters/releases/tag/v0.1.0

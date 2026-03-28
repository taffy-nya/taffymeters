# taffymeters

A lightweight audio visualization tool for Windows, inspired by MiniMeters.
Runs as a small always-on-top transparent window that sits over your desktop or other applications.

## Features

- Oscilloscope, spectrum analyzer, and spectrogram views
- Stereo goniometer (stereometer) for stereo field visualization
- Flexible panel layout: split any panel horizontally or vertically, drag dividers to resize
- Right-click any panel to switch its view or adjust settings
- Frameless, transparent, always-on-top window with drag-to-move and edge resize

## Building

Requires Rust 1.85 or later.

```
cargo build --release
```

The compiled binary will be at `target/release/taffymeters.exe`.

## Usage

Run the application and it will capture your default audio output device.
Drag the window anywhere on screen. Resize by dragging the window edges.

To split a panel, hover over its right or bottom edge and click the "+" that appears.
To change a panel's view or close it, right-click anywhere inside it.

## Project Structure

```
taffymeters/
  core/   Audio capture, ring buffer, FFT processing
  ui/     egui-based interface, panel layout, visualizations
```

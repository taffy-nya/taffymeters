# taffymeters

面向 Windows 的轻量音频可视化工具。当前重构已完成阶段 3：默认播放端点的
WASAPI loopback 同时驱动频域、时域和连续电平分析，并由可替换风格绘制。

## 当前能力

- 按设备实际格式建立有界系统音频采集
- 在独立 worker 中执行共享 Channel／FFT、有状态处理、时域历史和连续电平分析
- 使用 periodic Hann FFT 和 peak-dBFS 标定
- 支持 Left、Right、Mono mix 及设备描述中的全部离散声道
- 提供 Spectrum、Spectrogram、Oscilloscope、Waveform、Stereometer、Level Meter 和 Empty Module
- Spectrum 支持 Curve、Bars、Pixel 风格、峰值保持、悬停读数和范围缩放
- Oscilloscope 支持 Line 与独立实例的 Persistence 风格
- 可 resize、置顶、切换 Module／风格并支持暂停／恢复的原生窗口

BSP、多窗口 Workspace、Theme／持久化和产品外部插件加载仍属于后续阶段。
旧实现保留在 `tmp/prev_src`，只作为只读迁移参考。

## 构建

需要 Rust 1.85 或更高版本、Windows，以及可用的播放端点。

```text
cargo build --release
```

生成的程序位于 `target/release/taffymeters.exe`。

## 使用

运行程序后会采集默认音频播放端点。界面可选择 Module 及其支持的风格；
快捷键 `1`、`2`、`M` 分别选择 Left、Right 和 Mono average。
`Space` 暂停／恢复采集，`Escape` 关闭窗口。Spectrum 和 Oscilloscope
可在绘图区内使用滚轮调整显示范围。

## 项目结构

```text
taffymeters/
  src/audio/       输入描述、系统采集和有界块传输
  src/dataflow/    Channel、FFT、连续分析、History、worker 和快照
  src/builtins/    内置 Module 及其风格
  src/module.rs    Module／风格契约与注册
  src/plotting.rs  Spectrum 坐标、反向映射和刻度
  src/render.rs    宿主绘制接口和 egui 适配器
  src/app.rs       原生应用外壳
```

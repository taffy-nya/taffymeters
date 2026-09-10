# taffymeters

面向 Windows 的轻量音频可视化工具。当前重构阶段已跑通单条原生
Spectrum 路径，由默认播放端点的 WASAPI loopback 真实驱动。

## 当前能力

- 按设备实际格式建立有界系统音频采集
- 在独立 worker 中执行 `Input → Channel → FFT`
- 使用 periodic Hann FFT 和 peak-dBFS 标定
- 支持 Left、Right、Mono mix 及设备描述中的全部离散声道
- 通过注册入口创建 Spectrum Module 和可替换的默认折线风格
- 可 resize、置顶并支持暂停／恢复的原生窗口

旧视图和 BSP 布局保留在 `tmp/prev_src`，只作为后续迁移参考；
它们尚未在新架构中实现。

## 构建

需要 Rust 1.85 或更高版本、Windows，以及可用的播放端点。

```text
cargo build --release
```

生成的程序位于 `target/release/taffymeters.exe`。

## 使用

运行程序后会采集默认音频播放端点。界面会按设备实际声道数生成
全部声道选择入口；快捷键 `1`、`2`、`M` 分别选择 Left、Right 和
Mono average。`Space` 暂停／恢复采集，`Escape` 关闭窗口。

## 项目结构

```text
taffymeters/
  src/audio/       输入描述、系统采集和有界块传输
  src/dataflow/    Channel、标定 FFT、worker 和快照
  src/builtins/    注册式 Spectrum Module 和默认折线风格
  src/module.rs    Module／风格注册及 Spectrum 坐标
  src/render.rs    宿主绘制接口和 egui 适配器
  src/app.rs       原生应用外壳
```

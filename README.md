# taffymeters

面向 Windows 的轻量音频可视化工具。当前重构已完成阶段 4：默认播放端点的
WASAPI loopback 驱动按需频域／时域订阅，独立 Module 实例在多窗口 Workspace 的
BSP 布局中选择风格绘制。

## 当前能力

- 按设备实际格式建立有界系统音频采集
- 在独立 worker 中按订阅需求执行共享 Channel／FFT、私有有状态处理、时域历史和连续电平分析
- 使用 periodic Hann FFT 和 peak-dBFS 标定
- 支持 Left、Right、Mono mix 及设备描述中的全部离散声道
- 提供 Spectrum、Spectrogram、Oscilloscope、Waveform、Stereometer、Level Meter 和 Empty Module
- Spectrum 支持 Curve、Bars、Pixel 风格、峰值保持、悬停读数和范围缩放
- Oscilloscope 支持 Line 与独立实例的 Persistence 风格
- 多窗口 Workspace：窗口内 BSP 分割、拖动分割线（16 点网格，Alt 临时自由）、关闭叶子；
  Module 拆出为新窗口、移动／合并到任意窗口的叶子；窗口移动、resize、最大化、置顶、
  相互与屏幕边缘吸附、成组移动与解除

Theme／持久化和产品外部插件加载仍属于后续阶段。
旧实现保留在 `tmp/prev_src`，只作为只读迁移参考。

## 构建

需要 Rust 1.85 或更高版本、Windows，以及可用的播放端点。

```text
cargo build --release
```

生成的程序位于 `target/release/taffymeters.exe`。

## 使用

运行程序后会采集默认音频播放端点，打开一个无边框窗口。

- 悬停叶子右侧／底部边缘出现 `+`，单击向右／向下分割出同类型新实例；拖动分割线调整
  比例（按住 Alt 不吸附网格，双击复位均分）。
- 右键叶子打开菜单：Module（更换类型）、Channel、Style（含清空视觉状态）、
  Layout（分割、拆出到新窗口、移动到…）、Close module、Window（置顶、最大化、
  成组／解组、关闭窗口）、Source（状态与暂停）。“移动到…”后在任意窗口点击目标叶子的
  左／右／上／下目标带完成合并，Esc 或右键取消。
- 拖动正文移动窗口，拖动外缘 resize；松开时与其他窗口及屏幕工作区边缘吸附。
- 单击叶子选中；`1`、`2`、`M` 把选中且支持声道的实例设为 Left、Right、Mono average。
  `Space` 暂停／恢复整个音源；`Escape` 依次关闭菜单、取消拖动／移动／拆出，否则关闭
  当前窗口（`Ctrl+W` 同）。关闭最后一个窗口才退出程序。
- Spectrum 和 Oscilloscope 可在绘图区内使用滚轮调整显示范围。

## 项目结构

```text
taffymeters/
  src/audio/       输入描述、系统采集和有界块传输
  src/dataflow/    Channel、FFT、按需时域、连续分析和 worker
  src/builtins/    内置 Module 及其类型化风格
  src/module.rs    通用 Module／风格入口契约
  src/extensions.rs 注册表与工厂创建
  src/live.rs      采集和 worker 生命周期
  src/plotting.rs  Spectrum 坐标、反向映射和刻度
  src/render.rs    宿主绘制接口和 egui 适配器
  src/workspace/   Workspace、BSP 布局、窗口 UI 与桌面吸附
  src/app.rs       原生多窗口适配（egui viewport）
```

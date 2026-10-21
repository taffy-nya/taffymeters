use crate::{
    dataflow::{ChannelSelection, DataSpec, DataSubscription, Keep, OutputId, SOURCE},
    module::{
        MIN_SCROLL_POINTS, ModuleError, ModuleFrame, ModuleServices, WHEEL_ZOOM_PER_POINT,
        spec_from_saved,
    },
    settings::{SettingKind, SettingValue, ViewSetting, unit},
};

pub(super) fn pcm_spec(keep: Keep) -> (DataSpec, OutputId) {
    let mut spec = DataSpec::new();
    let output = spec.output(SOURCE, keep);
    (spec, output)
}

pub(super) fn subscribe_pcm(
    services: &ModuleServices,
    keep: Keep,
    saved: Option<&DataSpec>,
) -> Result<(DataSubscription, OutputId), ModuleError> {
    let (spec, output) = pcm_spec(keep);
    let spec = spec_from_saved(spec, saved)?;
    Ok((services.dataflow.subscribe(spec)?, output))
}

/// Display gain relative to the workspace reference, with a symmetric ±20 dB range.
pub(super) const SCALE_RANGE: (f32, f32) = (0.1, 10.0);
pub(super) const SCALE_KEY: &str = "scale";
pub(super) const CHANNEL_KEY: &str = "channel";

pub(super) fn scale_setting(value: f32) -> ViewSetting {
    ViewSetting {
        key: SCALE_KEY,
        label: "Scale",
        section: "Display",
        kind: SettingKind::Number {
            min: SCALE_RANGE.0,
            max: SCALE_RANGE.1,
            unit: unit::TIMES,
            logarithmic: true,
        },
        value: SettingValue::Number(value),
        default: SettingValue::Number(1.0),
    }
}

pub(super) fn wheel_scale(scale: f32, frame: &ModuleFrame<'_>) -> f32 {
    if frame
        .pointer
        .is_some_and(|point| frame.rect.contains(point))
        && frame.scroll_delta_y.abs() > MIN_SCROLL_POINTS
    {
        (scale * (frame.scroll_delta_y * WHEEL_ZOOM_PER_POINT).exp())
            .clamp(SCALE_RANGE.0, SCALE_RANGE.1)
    } else {
        scale
    }
}

pub(super) fn channel_setting(channel: ChannelSelection) -> ViewSetting {
    ViewSetting {
        key: CHANNEL_KEY,
        label: "Channel",
        section: "Display",
        kind: SettingKind::Channel,
        value: SettingValue::Channel(channel),
        default: SettingValue::Channel(ChannelSelection::MonoAverage),
    }
}

pub(super) fn channel_value(
    key: &str,
    value: &SettingValue,
) -> Result<ChannelSelection, ModuleError> {
    value
        .channel()
        .ok_or_else(|| ModuleError::InvalidSetting(key.to_owned()))
}

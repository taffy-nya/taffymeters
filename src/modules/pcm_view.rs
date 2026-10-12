//! Pieces shared by the built-in Modules that show source PCM (Oscilloscope, Waveform, Stereometer).

use crate::{
    dataflow::{ChannelSelection, DataSpec, DataSubscription, Keep, OutputId, SOURCE},
    module::{ModuleError, ModuleServices, spec_from_saved},
    settings::{SettingKind, SettingValue, ViewSetting},
};

/// A subscription to the merged source PCM (or the saved equivalent).
pub(super) fn subscribe_pcm(
    services: &ModuleServices,
    keep: Keep,
    saved: Option<&DataSpec>,
) -> Result<(DataSubscription, OutputId), ModuleError> {
    let mut spec = DataSpec::new();
    let output = spec.output(SOURCE, keep);
    let spec = spec_from_saved(spec, saved)?;
    Ok((services.dataflow.subscribe(spec)?, output))
}

pub(super) fn scale_setting(value: f32, range: (f32, f32)) -> ViewSetting {
    ViewSetting {
        key: "scale",
        label: "Scale",
        section: "Display",
        kind: SettingKind::Number {
            min: range.0,
            max: range.1,
            unit: "×",
            logarithmic: true,
        },
        value: SettingValue::Number(value),
    }
}

pub(super) fn channel_setting(channel: ChannelSelection) -> ViewSetting {
    ViewSetting {
        key: "channel",
        label: "Channel",
        section: "Display",
        kind: SettingKind::Channel,
        value: SettingValue::Channel(channel),
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

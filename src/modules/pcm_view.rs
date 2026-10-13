//! Pieces shared by the built-in Modules that show source PCM (Oscilloscope, Waveform, Stereometer).

use crate::{
    dataflow::{ChannelSelection, DataSpec, DataSubscription, Keep, OutputId, SOURCE},
    module::{ModuleError, ModuleServices, spec_from_saved},
    settings::{SettingKind, SettingValue, ViewSetting},
};

/// The source PCM output kept as `keep`: the whole analysis of these Modules.
pub(super) fn pcm_spec(keep: Keep) -> (DataSpec, OutputId) {
    let mut spec = DataSpec::new();
    let output = spec.output(SOURCE, keep);
    (spec, output)
}

/// A subscription to the merged source PCM (or the saved equivalent).
pub(super) fn subscribe_pcm(
    services: &ModuleServices,
    keep: Keep,
    saved: Option<&DataSpec>,
) -> Result<(DataSubscription, OutputId), ModuleError> {
    let (spec, output) = pcm_spec(keep);
    let spec = spec_from_saved(spec, saved)?;
    Ok((services.dataflow.subscribe(spec)?, output))
}

/// Every PCM view starts at unit scale.
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
        default: SettingValue::Number(1.0),
    }
}

/// Every PCM view starts with the mono average.
pub(super) fn channel_setting(channel: ChannelSelection) -> ViewSetting {
    ViewSetting {
        key: "channel",
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

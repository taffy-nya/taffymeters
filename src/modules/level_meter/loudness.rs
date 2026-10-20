//! Loudness Meter: one bar of the programme's 400 ms K-weighted momentary
//! loudness (BS.1770), on the absolute LUFS scale shared with the Level Meter.

use crate::{
    dataflow::{DataController, DataSpec, DataSubscription, Keep, NodeSpec, OutputId, SOURCE},
    extensions::{Registry, RegistryError},
    module::{
        Module, ModuleError, ModuleFactory, ModuleFrame, ModuleResponse, ModuleServices,
        spec_from_saved,
    },
    render::ColoredRect,
    snapshot::DataReader,
    theme::ColorRole,
};

use super::{Zones, bar_track, push_bar};

pub const LOUDNESS_METER_MODULE_ID: &str = "taffymeters.loudness_meter";

/// The colors the meters had at 70% and 85% of the −60…+3 scale.
const LOUDNESS_ZONES: Zones = Zones {
    mid_from: -15.9,
    high_above: -6.45,
};
/// The trough and the zone colors.
const COLOR_ROLES: &[ColorRole] = &[
    ColorRole::Surface,
    ColorRole::Low,
    ColorRole::Mid,
    ColorRole::High,
];

fn loudness_data() -> (DataSpec, OutputId) {
    let mut spec = DataSpec::new();
    let loudness = spec.add(SOURCE, NodeSpec::Loudness);
    let output = spec.output(loudness, Keep::Latest);
    (spec, output)
}

pub(super) struct LoudnessMeterFactory;

impl ModuleFactory for LoudnessMeterFactory {
    fn type_id(&self) -> &'static str {
        LOUDNESS_METER_MODULE_ID
    }

    fn display_name(&self) -> &'static str {
        "Loudness Meter"
    }

    fn create(
        &self,
        services: &ModuleServices,
        _registry: &Registry,
        data: Option<&DataSpec>,
    ) -> Result<Box<dyn Module>, RegistryError> {
        let (spec, output) = loudness_data();
        let spec = spec_from_saved(spec, data)?;
        let subscription = services
            .dataflow
            .subscribe(spec)
            .map_err(ModuleError::from)?;
        Ok(Box::new(LoudnessMeterModule {
            controller: subscription.controller(),
            reader: subscription.reader(),
            _subscription: subscription,
            output,
            rects: Vec::with_capacity(2),
        }))
    }

    fn color_roles(&self) -> &'static [ColorRole] {
        COLOR_ROLES
    }
}

struct LoudnessMeterModule {
    controller: DataController,
    reader: DataReader,
    _subscription: DataSubscription,
    output: OutputId,
    rects: Vec<ColoredRect>,
}

impl Module for LoudnessMeterModule {
    fn data(&self) -> Option<&DataController> {
        Some(&self.controller)
    }

    fn output_label(&self, output: OutputId) -> Option<&'static str> {
        (output == self.output).then_some("Loudness")
    }

    fn color_roles(&self) -> &[ColorRole] {
        COLOR_ROLES
    }

    fn draw(&mut self, frame: ModuleFrame<'_>) -> ModuleResponse {
        let Some(result) = self.reader.latest(self.output) else {
            return ModuleResponse::default();
        };
        let Some(loudness) = result.loudness() else {
            return ModuleResponse::default();
        };
        self.rects.clear();
        push_bar(
            &mut self.rects,
            frame.palette,
            LOUDNESS_ZONES,
            bar_track(frame.rect, 0, 1),
            loudness.momentary_lufs,
        );
        frame.renderer.fill_rects(frame.rect, &self.rects);
        ModuleResponse::default()
    }
}

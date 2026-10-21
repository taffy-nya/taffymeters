use super::{LINE_OSCILLOSCOPE_STYLE_ID, build_latest_trace};
use crate::{
    module::ModuleResponse,
    modules::oscilloscope::style::{
        OscilloscopeStyle, OscilloscopeStyleContext, OscilloscopeStyleFactory,
    },
    render::{Point, RenderTarget, Stroke},
    theme::ColorRole,
};

pub(super) struct LineOscilloscopeStyleFactory;

impl OscilloscopeStyleFactory for LineOscilloscopeStyleFactory {
    fn style_id(&self) -> &'static str {
        LINE_OSCILLOSCOPE_STYLE_ID
    }

    fn display_name(&self) -> &'static str {
        "Line"
    }

    fn create(&self) -> Box<dyn OscilloscopeStyle> {
        Box::new(LineOscilloscopeStyle {
            points: Vec::with_capacity(2048),
        })
    }
}

struct LineOscilloscopeStyle {
    points: Vec<Point>,
}

impl OscilloscopeStyle for LineOscilloscopeStyle {
    fn draw(
        &mut self,
        context: OscilloscopeStyleContext<'_>,
        renderer: &mut dyn RenderTarget,
    ) -> ModuleResponse {
        build_latest_trace(&context, &mut self.points);
        renderer.polyline(
            context.rect,
            &self.points,
            Stroke {
                width: 1.4,
                color: context.palette.color(ColorRole::Primary),
            },
        );
        // New PCM publication drives redraws; display time alone does not change the trace.
        ModuleResponse::default()
    }

    fn color_roles(&self) -> &'static [ColorRole] {
        &[ColorRole::Primary]
    }
}

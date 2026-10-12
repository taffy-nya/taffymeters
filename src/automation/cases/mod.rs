mod core;
mod settings;
mod visual;
mod workspace;

use super::{report::Report, support::run_case};

pub(in crate::automation) use visual::{compare_case, visual};

pub(super) fn core(report: &mut Report) {
    for (id, scenario) in core::CASES
        .into_iter()
        .chain(workspace::CASES)
        .chain(settings::CASES)
    {
        report.cases.push(run_case(id, scenario));
    }
}

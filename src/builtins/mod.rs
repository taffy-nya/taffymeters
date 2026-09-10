mod spectrum;

use crate::module::{Registry, RegistryError};

pub use spectrum::{DEFAULT_SPECTRUM_STYLE_ID, SPECTRUM_MODULE_ID};

pub fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    spectrum::register(registry)
}

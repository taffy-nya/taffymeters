#[cfg(feature = "automation")]
pub(crate) mod synthetic;
mod system;

pub(crate) use system::{
    Endpoint, EndpointInfo, Endpoints, OpenSystemInput, SystemCapture, list_endpoints,
};

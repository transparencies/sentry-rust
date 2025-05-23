// Not all constants are used when building without the "client" feature
#![allow(dead_code)]

use std::sync::LazyLock;

use crate::protocol::{ClientSdkInfo, ClientSdkPackage};

/// The version of the library
const VERSION: &str = env!("CARGO_PKG_VERSION");
pub(crate) const USER_AGENT: &str = concat!("sentry.rust/", env!("CARGO_PKG_VERSION"));

pub(crate) static SDK_INFO: LazyLock<ClientSdkInfo> = LazyLock::new(|| ClientSdkInfo {
    name: "sentry.rust".into(),
    version: VERSION.into(),
    packages: vec![ClientSdkPackage {
        name: "cargo:sentry".into(),
        version: VERSION.into(),
    }],
    integrations: vec![],
});

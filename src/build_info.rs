pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const BUILD: &str = env!("ARTINCHIP_FLASH_BUILD");
pub const COMMIT: &str = env!("ARTINCHIP_FLASH_COMMIT");

pub const VERSION_WITH_BUILD: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (build ",
    env!("ARTINCHIP_FLASH_BUILD"),
    ")"
);

pub const LONG_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "\nbuild: ",
    env!("ARTINCHIP_FLASH_BUILD"),
    "\ncommit: ",
    env!("ARTINCHIP_FLASH_COMMIT")
);

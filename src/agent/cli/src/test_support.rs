#![allow(dead_code, unused_imports)]

#[path = "../product/test_support/cargo_bin/src/lib.rs"]
pub(crate) mod cargo_bin;

#[path = "../product/test_support/core/lib.rs"]
pub(crate) mod core;

#[path = "../product/test_support/app_server/lib.rs"]
pub(crate) mod app_server;

#[path = "../product/test_support/mcp_server/lib.rs"]
pub(crate) mod mcp_server;

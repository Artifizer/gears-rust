//! Domain layer.
// The repository port takes `toolkit_db` runner and scope types, like the
// simple-user-settings gear this example follows.
#![allow(unknown_lints)]
#![allow(de0301_no_infra_in_domain)]

pub mod error;
pub mod governance;
pub mod local_client;
pub mod repo;
pub mod service;

//! Health evaluation, replication observations, history facts and derived calculations.
pub mod derived;
pub mod history;
pub mod models;
pub mod replication;
pub mod rules;

use models::Status;

#[derive(Clone, Debug)]
pub struct Health {
    pub overall: Status,
    pub stratum0: Status,
    pub stratum1: Status,
    pub syncservers: Status,
}

pub mod observations;

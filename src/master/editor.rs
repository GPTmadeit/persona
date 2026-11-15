use nih_plug::prelude::*;
use std::sync::Arc;

use super::params::MasterParams;
use super::shared::MasterShared;

pub const WIDTH: u32 = 1240;
pub const HEIGHT: u32 = 780;

pub fn create(_params: Arc<MasterParams>, _shared: Arc<MasterShared>) -> Option<Box<dyn Editor>> {
    None
}

// SPDX-License-Identifier: AGPL-3.0-or-later

#![no_std]

mod device;
mod error;
mod state;

pub use device::{GicConfig, GicFrame, Gicv3};
pub use error::GicError;
pub use state::{
    CNTV_CTL_ENABLE, CNTV_CTL_IMASK, GIC_MAX_INTID, GIC_STATE_VERSION, GICD_FRAME_SIZE,
    GICR_FRAME_SIZE, GicState, SGI_PPI_COUNT,
};

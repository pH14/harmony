// SPDX-License-Identifier: AGPL-3.0-or-later

use core::fmt;

use crate::exit::{CommonExit, ExitReason};

pub mod arm64;
pub mod x86;

pub trait Arch {
    type Exit: ArchExit;
    type Injection: Copy + fmt::Debug + PartialEq;
    type VcpuState: Clone + fmt::Debug + PartialEq + Default;
    type Policy: Clone + fmt::Debug + PartialEq;
    type IntId: Copy + fmt::Debug + PartialEq;
    type Caps: Copy + fmt::Debug + PartialEq;
    type Completion: fmt::Debug;

    fn stages_common_completion(exit: &CommonExit) -> bool {
        exit.stages_completion()
    }
}

pub trait ArchExit: Clone + fmt::Debug + PartialEq {
    fn reason(&self) -> ExitReason;
    fn stages_completion(&self) -> bool;
}

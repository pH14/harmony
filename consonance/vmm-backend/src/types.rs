// SPDX-License-Identifier: AGPL-3.0-or-later

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[repr(transparent)]
pub struct Gpa(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MpState {
    #[default]
    Runnable,
    Halted,
}

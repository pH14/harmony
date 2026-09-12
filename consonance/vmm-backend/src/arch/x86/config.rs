// SPDX-License-Identifier: AGPL-3.0-or-later

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct CpuidModel {
    pub entries: Vec<CpuidEntry>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct CpuidEntry {
    pub leaf: u32,
    pub subleaf: u32,
    pub subleaf_significant: bool,
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct MsrFilter {
    pub allow_inkernel: Vec<MsrRange>,
}

impl MsrFilter {
    pub fn allow_indices(&self) -> impl Iterator<Item = u32> + '_ {
        self.allow_inkernel
            .iter()
            .flat_map(|r| r.base..r.base.saturating_add(r.count))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct MsrRange {
    pub base: u32,
    pub count: u32,
}

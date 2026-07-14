// SPDX-License-Identifier: AGPL-3.0-or-later
//! Portable configuration POD installed once before the first run.
//!
//! vmm-core builds both [`CpuidModel`] and [`MsrFilter`] from
//! `docs/CPU-MSR-CONTRACT.md`; the backend never invents the data, it only
//! installs what it is handed (`KVM_SET_CPUID2` / `KVM_X86_SET_MSR_FILTER` on
//! KVM). Defining them here keeps vmm-core impl-agnostic: it pushes policy
//! through the trait rather than reaching for a KVM ioctl. No `cfg`, no `unsafe`.

/// The frozen guest-visible CPUID table (→ `KVM_SET_CPUID2` on KVM). One entry
/// per `(leaf, subleaf)` the contract enumerates. Deterministic: equal model ⇒
/// equal bytes, and the impl emits entries to KVM in this `Vec` order.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct CpuidModel {
    /// The CPUID entries, in the order the impl installs them.
    pub entries: Vec<CpuidEntry>,
}

/// One `(leaf, subleaf)` CPUID result. Flat POD.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct CpuidEntry {
    /// CPUID leaf (`EAX` input).
    pub leaf: u32,
    /// CPUID subleaf (`ECX` input).
    pub subleaf: u32,
    /// Whether `subleaf` is significant for this leaf (→
    /// `KVM_CPUID_FLAG_SIGNIFICANT_INDEX`). `false` for leaves whose result
    /// ignores `ECX`.
    pub subleaf_significant: bool,
    /// Result `EAX`.
    pub eax: u32,
    /// Result `EBX`.
    pub ebx: u32,
    /// Result `ECX`.
    pub ecx: u32,
    /// Result `EDX`.
    pub edx: u32,
}

/// The default-deny MSR policy (→ `KVM_X86_SET_MSR_FILTER`, installed *after* the
/// `KVM_CAP_X86_USER_SPACE_MSR` `FILTER|UNKNOWN|INVAL` mask). It names ONLY the
/// MSRs KVM may keep servicing **in-kernel** (the contract's "KVM virtualizes
/// this correctly" set). Every MSR outside these ranges — and every
/// unknown/invalid MSR — traps to userspace as `Exit::Rdmsr`/`Exit::Wrmsr`,
/// where vmm-core applies the contract disposition. The *disposition* lives in
/// vmm-core; this filter only decides in-kernel-vs-userspace.
///
/// This same set is what `save`/`restore` enumerate over `KVM_GET/SET_MSRS` (the
/// `allow-stateful` rows): `KvmBackend` retains the filter and walks these
/// ranges' indices, so `VcpuState.msrs` round-trips exactly that set.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct MsrFilter {
    /// Sorted, non-overlapping ranges KVM keeps servicing in-kernel.
    pub allow_inkernel: Vec<MsrRange>,
}

impl MsrFilter {
    /// Iterate every MSR index named by the in-kernel allow ranges, in
    /// ascending order (deterministic — the ranges are sorted and
    /// non-overlapping). This is the `allow-stateful` index list `save`/`restore`
    /// walk; deduplicating the iterator is unnecessary because the ranges do not
    /// overlap.
    pub fn allow_indices(&self) -> impl Iterator<Item = u32> + '_ {
        self.allow_inkernel
            .iter()
            .flat_map(|r| r.base..r.base.saturating_add(r.count))
    }
}

/// A half-open MSR-index range `[base, base + count)`. Ranges in an [`MsrFilter`]
/// are sorted and non-overlapping (deterministic; the impl folds them into KVM's
/// `kvm_msr_filter_range` bitmaps).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct MsrRange {
    /// First MSR index in the range.
    pub base: u32,
    /// Number of consecutive indices (range is `[base, base + count)`).
    pub count: u32,
}

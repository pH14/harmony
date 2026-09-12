// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::{Arc, atomic::AtomicBool};

use crate::arch::Arch;
use crate::error::Result;
use crate::exit::{Capabilities, Exit, ExitCounts};
use crate::types::Gpa;

pub trait Backend {
    type A: Arch;

    fn set_policy(&mut self, policy: &<Self::A as Arch>::Policy) -> Result<()>;

    /// # Safety
    /// The caller MUST guarantee that `host`'s backing (a) stays live at a fixed
    /// address — pinned, never reallocated or moved — until the backend is
    /// dropped or the region is replaced; (b) is not aliased by any other live
    /// `&`/`&mut` while a `run` is in flight; and (c) starts at a
    /// **4 KiB-aligned host address** (`host.as_ptr() as usize % 4096 == 0`).
    /// `KVM_SET_USER_MEMORY_REGION` requires the *userspace address itself* to be
    /// page-aligned, which a plain `Vec<u8>`/slice does NOT guarantee (KVM
    /// rejects it with `EINVAL`) — back the region with an `mmap`/page-aligned
    /// allocation. Violating (a)/(b) is a use-after-free or data race; that
    /// unenforceable invariant is why this is `unsafe`. The backend records the
    /// region and retains `host`'s pointer past this call — the `&mut [u8]`
    /// borrow ends at return, but the guest writes through that pointer during
    /// every later `run`.
    unsafe fn map_memory(&mut self, gpa: Gpa, host: &mut [u8]) -> Result<()>;

    fn drain_dirty_pages(&mut self) -> Result<Vec<u64>> {
        Err(crate::error::BackendError::Unsupported {
            what: "drain_dirty_pages",
        })
    }

    fn run(&mut self) -> Result<Exit<Self::A>>;

    fn inject(&mut self, event: <Self::A as Arch>::Injection) -> Result<()>;

    fn set_pending_irq(&mut self, id: Option<<Self::A as Arch>::IntId>) -> Result<()>;

    fn take_accepted_interrupt(&mut self) -> Option<<Self::A as Arch>::IntId>;

    fn complete_read(&mut self, value: u64) -> Result<()>;

    fn complete_fault(&mut self) -> Result<()>;

    fn complete_ok(&mut self) -> Result<()>;

    fn complete_hypercall(&mut self, ret: u64) -> Result<()>;

    fn complete_arch(&mut self, completion: <Self::A as Arch>::Completion) -> Result<()>;

    fn retire_pending_completion(&mut self) -> Result<()> {
        Err(crate::error::BackendError::Unsupported {
            what: "retire_pending_completion",
        })
    }

    fn save(&self) -> Result<<Self::A as Arch>::VcpuState>;

    fn restore(&mut self, state: &<Self::A as Arch>::VcpuState) -> Result<()>;

    fn exit_counts(&self) -> ExitCounts;

    fn reset_exit_counts(&mut self);

    fn capabilities(&self) -> Capabilities<<Self::A as Arch>::Caps>;

    fn cancellation_flag(&self) -> Option<Arc<AtomicBool>> {
        None
    }
}

impl<B: Backend + ?Sized> Backend for Box<B> {
    type A = B::A;

    fn set_policy(&mut self, policy: &<Self::A as Arch>::Policy) -> Result<()> {
        (**self).set_policy(policy)
    }

    unsafe fn map_memory(&mut self, gpa: Gpa, host: &mut [u8]) -> Result<()> {
        // SAFETY: the caller upholds `map_memory`'s contract; we only forward the
        // call to the boxed backend, adding no new obligation.
        unsafe { (**self).map_memory(gpa, host) }
    }

    fn drain_dirty_pages(&mut self) -> Result<Vec<u64>> {
        (**self).drain_dirty_pages()
    }

    fn run(&mut self) -> Result<Exit<Self::A>> {
        (**self).run()
    }

    fn inject(&mut self, event: <Self::A as Arch>::Injection) -> Result<()> {
        (**self).inject(event)
    }

    fn set_pending_irq(&mut self, id: Option<<Self::A as Arch>::IntId>) -> Result<()> {
        (**self).set_pending_irq(id)
    }

    fn take_accepted_interrupt(&mut self) -> Option<<Self::A as Arch>::IntId> {
        (**self).take_accepted_interrupt()
    }

    fn complete_read(&mut self, value: u64) -> Result<()> {
        (**self).complete_read(value)
    }

    fn complete_fault(&mut self) -> Result<()> {
        (**self).complete_fault()
    }

    fn complete_ok(&mut self) -> Result<()> {
        (**self).complete_ok()
    }

    fn complete_hypercall(&mut self, ret: u64) -> Result<()> {
        (**self).complete_hypercall(ret)
    }

    fn complete_arch(&mut self, completion: <Self::A as Arch>::Completion) -> Result<()> {
        (**self).complete_arch(completion)
    }

    fn retire_pending_completion(&mut self) -> Result<()> {
        (**self).retire_pending_completion()
    }

    fn save(&self) -> Result<<Self::A as Arch>::VcpuState> {
        (**self).save()
    }

    fn restore(&mut self, state: &<Self::A as Arch>::VcpuState) -> Result<()> {
        (**self).restore(state)
    }

    fn exit_counts(&self) -> ExitCounts {
        (**self).exit_counts()
    }

    fn reset_exit_counts(&mut self) {
        (**self).reset_exit_counts()
    }

    fn capabilities(&self) -> Capabilities<<Self::A as Arch>::Caps> {
        (**self).capabilities()
    }

    fn cancellation_flag(&self) -> Option<Arc<AtomicBool>> {
        (**self).cancellation_flag()
    }
}

#[cfg(test)]
mod tests {
    use super::Backend;
    use crate::arch::x86::{Injection, VcpuState, X86, X86Caps, X86Completion, X86Policy};
    use crate::error::{BackendError, Result};
    use crate::exit::{Capabilities, Exit, ExitCounts};
    use crate::types::Gpa;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    #[derive(Default)]
    struct DefaultRetireBackend;

    impl Backend for DefaultRetireBackend {
        type A = X86;

        fn set_policy(&mut self, _policy: &X86Policy) -> Result<()> {
            Ok(())
        }

        unsafe fn map_memory(&mut self, _gpa: Gpa, _host: &mut [u8]) -> Result<()> {
            Err(BackendError::Unsupported {
                what: "default-retire-test map_memory",
            })
        }

        fn run(&mut self) -> Result<Exit<X86>> {
            Err(BackendError::Unsupported {
                what: "default-retire-test run",
            })
        }

        fn inject(&mut self, _event: Injection) -> Result<()> {
            Ok(())
        }

        fn set_pending_irq(&mut self, _id: Option<u8>) -> Result<()> {
            Ok(())
        }

        fn take_accepted_interrupt(&mut self) -> Option<u8> {
            None
        }

        fn complete_read(&mut self, _value: u64) -> Result<()> {
            Err(BackendError::BadCompletion)
        }

        fn complete_fault(&mut self) -> Result<()> {
            Err(BackendError::BadCompletion)
        }

        fn complete_ok(&mut self) -> Result<()> {
            Err(BackendError::BadCompletion)
        }

        fn complete_hypercall(&mut self, _ret: u64) -> Result<()> {
            Err(BackendError::BadCompletion)
        }

        fn complete_arch(&mut self, _completion: X86Completion) -> Result<()> {
            Err(BackendError::BadCompletion)
        }

        fn save(&self) -> Result<VcpuState> {
            Ok(VcpuState::default())
        }

        fn restore(&mut self, _state: &VcpuState) -> Result<()> {
            Ok(())
        }

        fn exit_counts(&self) -> ExitCounts {
            ExitCounts::default()
        }

        fn reset_exit_counts(&mut self) {}

        fn capabilities(&self) -> Capabilities<X86Caps> {
            Capabilities {
                name: "default-retire-test",
                arch: X86Caps,
            }
        }
    }

    #[test]
    fn default_retirement_is_fail_closed_and_box_forwards_it() {
        let mut plain = DefaultRetireBackend;
        assert!(matches!(
            plain.retire_pending_completion(),
            Err(BackendError::Unsupported {
                what: "retire_pending_completion"
            })
        ));

        let mut boxed: Box<dyn Backend<A = X86>> = Box::new(DefaultRetireBackend);
        assert!(matches!(
            boxed.retire_pending_completion(),
            Err(BackendError::Unsupported {
                what: "retire_pending_completion"
            })
        ));
    }

    #[test]
    fn a_backend_without_a_latch_reports_none_through_a_box_too() {
        assert!(DefaultRetireBackend.cancellation_flag().is_none());
        let without: Box<dyn Backend<A = X86>> = Box::new(DefaultRetireBackend);
        assert!(without.cancellation_flag().is_none());
    }

    #[cfg(feature = "mock")]
    #[test]
    fn box_forwards_the_backend_latch_unchanged() {
        let latch = Arc::new(AtomicBool::new(false));
        let boxed: Box<dyn Backend<A = X86>> =
            Box::new(crate::MockBackend::new().with_cancellation_flag(Arc::clone(&latch)));
        let forwarded = boxed.cancellation_flag().expect("latch forwarded");
        assert!(Arc::ptr_eq(&forwarded, &latch));
    }
}

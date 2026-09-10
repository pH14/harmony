#!/usr/bin/env python3
"""Observe the pinned reference; optionally test completion-only retirement."""
from pathlib import Path
import sys

root = Path(sys.argv[1])

def replace(path, old, new):
    target = root / path
    source = target.read_text()
    if source.count(old) != 1:
        raise SystemExit(f"expected one instrumentation site: {path}: {old}")
    target.write_text(source.replace(old, new))

replace('consonance/vmm-core/src/control.rs',
    'Err(VmmError::ContractViolation(_)) => return Ok(Err(ControlError::NotQuiescent)),',
    '''Err(VmmError::ContractViolation(reason)) => {
                eprintln!("SNAPSHOT_REFUSED at={:?} reason={reason}", vmm.effective_vns());
                return Ok(Err(ControlError::NotQuiescent));
            },''')
replace('consonance/client/src/session/live.rs',
    '''        self.run_until(deadline)
    }

    /// Issue one control request''',
    '''        let stop = self.run_until(deadline)?;
        eprintln!("SNAPSHOT_SETTLE from={now} requested={deadline} stop={stop:?}");
        Ok(stop)
    }

    /// Issue one control request''')
replace('consonance/vmm-core/src/vmm.rs',
    '    pub fn reseed_entropy(&mut self, seed: u64) -> Result<(), VmmError> {',
    '''    pub fn reseed_entropy(&mut self, seed: u64) -> Result<(), VmmError> {
        eprintln!("ENTROPY_RESEED at={:?} seed={seed}", self.effective_vns());''')
replace('consonance/vmm-core/src/control.rs',
    '    pub fn handle(&mut self, req: &Request) -> Result<Result<Reply, ControlError>, ServeError> {',
    '''    pub fn handle(&mut self, req: &Request) -> Result<Result<Reply, ControlError>, ServeError> {
        let operation = match req {
            Request::Snapshot => "snapshot",
            Request::Run { .. } => "run",
            Request::Branch { .. } => "branch",
            Request::Replay { .. } => "replay",
            _ => return self.handle_uninstrumented(req),
        };
        if let Some(vmm) = self.vmm.as_ref() {
            eprintln!("CONTROL_BEFORE op={operation} at={:?}", vmm.effective_vns());
        }
        let result = self.handle_uninstrumented(req);
        eprintln!("CONTROL_REPLY op={operation} result={result:?}");
        if let Some(vmm) = self.vmm.as_ref() {
            eprintln!("CONTROL_AFTER op={operation} at={:?} components={:x?}",
                vmm.effective_vns(), vmm.state_components());
            eprintln!("COMPLETION op={operation} staged={} last_exit={:?}",
                vmm.completion_staged, vmm.diagnostic_last_exit);
            eprintln!("STATE_SUFFIX op={operation} at={:?} bytes={:02x?}",
                vmm.effective_vns(), vmm.state_blob_suffix());
        }
        result
    }

    fn handle_uninstrumented(&mut self, req: &Request) -> Result<Result<Reply, ControlError>, ServeError> {''')

replace('consonance/vmm-core/src/vmm.rs',
    '    pub(crate) completion_staged: bool,',
    '    pub(crate) completion_staged: bool,\n    pub(crate) diagnostic_last_exit: Option<String>,')
replace('consonance/vmm-core/src/vmm.rs',
    '            completion_staged: false,',
    '            completion_staged: false,\n            diagnostic_last_exit: None,')
replace('consonance/vmm-core/src/vmm.rs',
    '        self.completion_staged = exit.stages_completion();',
    '        self.completion_staged = exit.stages_completion();\n        self.diagnostic_last_exit = Some(format!("{:?}", exit.reason()));')

if "--retire-completions" in sys.argv[2:]:
    replace('consonance/vmm-core/src/vmm.rs',
        '        <B::A as Vendor>::post_exit(self)?;',
        '''        <B::A as Vendor>::post_exit(self)?;
        // Diagnostic experiment: finish only the already serviced backend
        // operation before exposing a stopped step. Keep the existing SDK
        // snapshot-point latch so this experiment isolates CPU completion.
        if self.completion_staged {
            self.backend.retire_pending_completion()?;
            self.completion_staged = false;
        }''')

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

if "--check-execution-identity" in sys.argv[2:]:
    replace('.github/scripts/historical-investigate.sh',
        '''[[ $(jq -cS '.properties, .virtual_time_nanos, .state_hash' <<<"${inspected}") \\
   == $(jq -cS '.properties, .virtual_time_nanos, .state_hash' <<<"${after}") ]] \\
''',
        '''inspection_before=$(jq -cS '[.properties, .virtual_time_nanos, .state_hash]' <<<"${inspected}")
inspection_after=$(jq -cS '[.properties, .virtual_time_nanos, .state_hash]' <<<"${after}")
[[ "${inspection_before}" == "${inspection_after}" ]] \\
''')
    replace('.github/scripts/historical-investigate.sh',
        """whole_time=$(jq -r '.virtual_time_nanos' "reports/${CASE_ID}.run-whole.json")""",
        '''whole_time=$(jq -r '.virtual_time_nanos' "reports/${CASE_ID}.run-whole.json")
[[ "${whole_hash}" == "${target_hash}" && "${whole_time}" == "${target_time}" ]] \\
    && record "original and cold continuation have identical state and moment" 0 "${whole_hash}@${whole_time}" \\
    || record "original and cold continuation have identical state and moment" 1 "original ${target_hash}@${target_time}; cold ${whole_hash}@${whole_time}"''')

if "--check-execution-identity" in sys.argv[2:]:
    replace('workloads/faults/src/consonance.rs',
        '''            live.session
                .state_hash()
                .map_err(|error| format!("state hash: {error}"))''',
        '''            let hash = live.session.state_hash()
                .map_err(|error| format!("state hash: {error}"))?;
            let events = live.session.sdk_events()
                .map_err(|error| format!("diagnostic events: {error}"))?;
            let hex: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
            std::fs::write(format!("reports/original-sdk-events-{hex}-{}.json", std::process::id()),
                serde_json::to_vec(&events).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
            Ok(hash)''')
    replace('workloads/faults/src/investigate/live.rs',
        '''        Ok(hash.iter().map(|byte| format!("{byte:02x}")).collect())''',
        '''        let hex: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
        let events = self.session.sdk_events()
            .map_err(|error| format!("diagnostic events: {error}"))?;
        std::fs::write(format!("reports/continuation-sdk-events-{hex}-{}.json", std::process::id()),
            serde_json::to_vec(&events).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        Ok(hex)''')

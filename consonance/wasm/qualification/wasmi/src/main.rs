// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, env, fs, time::Instant};
use wasmi::{
    Caller, Config, Engine, Func, FuncRef, HarmonyContinuation, Instance, Linker, Memory, Module,
    ResumableCall, Store, Val,
};

const FUEL: u64 = 1_000_000_000_000;

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
struct Host {
    pending: Option<i32>,
    effects: Vec<(i32, i32)>,
    stop_import: bool,
    fuel_supplied: u64,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Captured {
    module: Vec<u8>,
    memory: Vec<u8>,
    globals: BTreeMap<String, (u8, u64)>,
    tables: BTreeMap<String, Vec<Option<String>>>,
    fuel: u64,
    host: Host,
    values: Vec<u64>,
    frames: Vec<[u64; 5]>,
    required_fuel: Option<u64>,
    caller_result: Option<i16>,
    data_segments: Vec<usize>,
    element_segments: Vec<u32>,
}

struct Runtime {
    engine: Engine,
    instance: Instance,
    store: Store<Host>,
    memory: Memory,
    root: Func,
    host_func: Func,
    module: Vec<u8>,
}

fn scalar(value: Val) -> (u8, u64) {
    match value {
        Val::I32(value) => (0, value as u32 as u64),
        Val::I64(value) => (1, value as u64),
        Val::F32(value) => (2, value.to_bits() as u64),
        Val::F64(value) => (3, value.to_bits()),
        _ => panic!("reference-valued globals are outside the profile"),
    }
}

fn value((kind, bits): (u8, u64)) -> Val {
    match kind {
        0 => Val::I32(bits as i32),
        1 => Val::I64(bits as i64),
        2 => Val::F32(wasmi::core::F32::from_bits(bits as u32)),
        3 => Val::F64(wasmi::core::F64::from_bits(bits)),
        _ => panic!("invalid scalar kind"),
    }
}

impl Runtime {
    fn new(module_bytes: &[u8]) -> Result<Self, Box<dyn std::error::Error>> {
        let mut config = Config::default();
        config.consume_fuel(true);
        let engine = Engine::new(&config);
        let module = Module::new(&engine, module_bytes)?;
        let mut store = Store::new(&engine, Host::default());
        store.set_fuel(FUEL)?;
        let host_func = Func::wrap(
            &mut store,
            |mut caller: Caller<'_, Host>, suggestion: i32| -> Result<i32, wasmi::Error> {
                if caller.data().stop_import {
                    caller.data_mut().pending = Some(suggestion);
                    return Err(wasmi::Error::new("pending decision"));
                }
                caller.data_mut().effects.push((suggestion, suggestion));
                Ok(suggestion)
            },
        );
        let mut linker = Linker::new(&engine);
        linker.define("harmony_v1", "decision", host_func)?;
        linker.func_wrap("wasi_snapshot_preview1", "fd_close", |_: i32| -> i32 { 8 })?;
        linker.func_wrap(
            "wasi_snapshot_preview1",
            "fd_seek",
            |_: i32, _: i64, _: i32, _: i32| -> i32 { 8 },
        )?;
        linker.func_wrap(
            "wasi_snapshot_preview1",
            "fd_write",
            |_: i32, _: i32, _: i32, _: i32| -> Result<i32, wasmi::Error> {
                Err(wasmi::Error::new(
                    "unexpected console effect in Wasmi workload probe",
                ))
            },
        )?;
        let instance = linker
            .instantiate(&mut store, &module)?
            .ensure_no_start(&mut store)?;
        let memory = instance
            .get_memory(&store, "memory")
            .ok_or("missing memory")?;
        let root = instance.get_func(&store, "run").ok_or("missing run")?;
        Ok(Self {
            engine,
            instance,
            store,
            memory,
            root,
            host_func,
            module: module_bytes.to_vec(),
        })
    }

    fn initialize(&mut self, rom: Option<&[u8]>) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(init) = self.instance.get_func(&self.store, "_initialize") {
            init.call(&mut self.store, &[], &mut [])?;
        }
        if let Some(rom) = rom {
            let pointer = self
                .instance
                .get_typed_func::<(), i32>(&self.store, "rom_address")?
                .call(&mut self.store, ())?;
            self.memory.write(&mut self.store, pointer as usize, rom)?;
            let result = self
                .instance
                .get_typed_func::<i32, i32>(&self.store, "initialize")?
                .call(&mut self.store, rom.len() as i32)?;
            if result != 0 {
                return Err(format!("QuickNES initialization failed: {result}").into());
            }
        }
        self.store.set_fuel(FUEL)?;
        self.store.data_mut().fuel_supplied = FUEL;
        Ok(())
    }

    fn capture(
        &mut self,
        call: Option<&ResumableCall>,
    ) -> Result<Captured, Box<dyn std::error::Error>> {
        let functions: BTreeMap<String, String> = self
            .instance
            .exports(&self.store)
            .filter_map(|export| {
                export
                    .clone()
                    .into_func()
                    .map(|func| (export.name().to_owned(), format!("{func:?}")))
            })
            .filter(|(name, _)| name.starts_with("__harmony_func_"))
            .map(|(name, key)| (key, name))
            .collect();
        let mut globals = BTreeMap::new();
        let mut tables = BTreeMap::new();
        for export in self.instance.exports(&self.store) {
            let name = export.name();
            if name.starts_with("__harmony_global_") {
                let global = export.clone().into_global().ok_or("wrong global export")?;
                if global.ty(&self.store).mutability().is_mut() {
                    globals.insert(name.to_owned(), scalar(global.get(&self.store)));
                }
            } else if name.starts_with("__harmony_table_") {
                let table = export.into_table().ok_or("wrong table export")?;
                let entries = (0..table.size(&self.store))
                    .map(|index| {
                        table
                            .get(&self.store, index)
                            .unwrap()
                            .funcref()
                            .unwrap()
                            .func()
                            .map(|func| functions[&format!("{func:?}")].clone())
                    })
                    .collect();
                tables.insert(name.to_owned(), entries);
            }
        }
        let continuation = call.map(ResumableCall::harmony_capture).transpose()?;
        let (values, frames, required_fuel, caller_result) = continuation
            .map(|c| (c.values, c.frames, c.required_fuel, c.caller_result))
            .unwrap_or_default();
        Ok(Captured {
            module: Sha256::digest(&self.module).to_vec(),
            memory: self.memory.data(&self.store).to_vec(),
            data_segments: self.instance.harmony_segment_lengths(&mut self.store).0,
            element_segments: self.instance.harmony_segment_lengths(&mut self.store).1,
            globals,
            tables,
            fuel: self.store.get_fuel()?,
            host: self.store.data().clone(),
            values,
            frames,
            required_fuel,
            caller_result,
        })
    }

    fn restore(
        &mut self,
        captured: &Captured,
    ) -> Result<ResumableCall, Box<dyn std::error::Error>> {
        if Sha256::digest(&self.module).as_slice() != captured.module {
            return Err("module mismatch".into());
        }
        self.memory.write(&mut self.store, 0, &captured.memory)?;
        for (name, scalar) in &captured.globals {
            self.instance
                .get_global(&self.store, name)
                .ok_or("missing global")?
                .set(&mut self.store, value(*scalar))?;
        }
        for (name, entries) in &captured.tables {
            let table = self
                .instance
                .get_table(&self.store, name)
                .ok_or("missing table")?;
            for (index, entry) in entries.iter().enumerate() {
                let func = entry
                    .as_ref()
                    .map(|name| self.instance.get_func(&self.store, name).unwrap());
                table.set(
                    &mut self.store,
                    index as u64,
                    Val::FuncRef(FuncRef::new(func)),
                )?;
            }
        }
        self.instance.harmony_restore_segments(
            &mut self.store,
            &captured.data_segments,
            &captured.element_segments,
        )?;
        self.store.set_fuel(captured.fuel)?;
        *self.store.data_mut() = captured.host.clone();
        let continuation = HarmonyContinuation {
            values: captured.values.clone(),
            frames: captured.frames.clone(),
            required_fuel: captured.required_fuel,
            caller_result: captured.caller_result,
        };
        // SAFETY: This qualification-only runner restores trusted captures with only controlled scalar or function-index changes, of the same scalar-only module and eager translation configuration. It does not accept untrusted artifacts.
        let call = unsafe {
            ResumableCall::harmony_restore(
                self.engine.clone(),
                self.instance,
                self.root,
                self.host_func,
                &continuation,
            )?
        };
        Ok(call)
    }
}

fn changed_pages(previous: &[u8], current: &[u8]) -> Vec<usize> {
    assert_eq!(previous.len(), current.len());
    previous
        .chunks(4096)
        .zip(current.chunks(4096))
        .enumerate()
        .filter_map(|(page, (before, after))| (before != after).then_some(page))
        .collect()
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[allow(clippy::disallowed_methods)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args().skip(1).collect();
    if args.len() < 4 {
        return Err("usage: probe MODULE MODE STATE REPORT [ROM FRAMES]".into());
    }
    let module = fs::read(&args[0])?;
    let mode = &args[1];
    let rom = args.get(4).map(fs::read).transpose()?;
    let frames: i32 = args.get(5).map(|s| s.parse()).transpose()?.unwrap_or(120);
    let start = Instant::now();
    let mut runtime = Runtime::new(&module)?;
    let mut output = [Val::I32(0)];
    let setup;
    let run_start;
    let call;
    let baseline;
    if mode == "resume" {
        let captured: Captured = postcard::from_bytes(&fs::read(&args[2])?)?;
        let restored = runtime.restore(&captured)?;
        baseline = Some(captured.memory);
        setup = start.elapsed();
        run_start = Instant::now();
        runtime.store.data_mut().stop_import = false;
        call = match restored {
            ResumableCall::OutOfFuel(trap) => {
                let consumed = runtime.store.data().fuel_supplied - runtime.store.get_fuel()?;
                runtime.store.set_fuel(FUEL - consumed)?;
                runtime.store.data_mut().fuel_supplied = FUEL;
                trap.resume(&mut runtime.store, &mut output)?
            }
            ResumableCall::HostTrap(trap) => {
                let suggestion = runtime
                    .store
                    .data_mut()
                    .pending
                    .take()
                    .ok_or("missing pending request")?;
                runtime
                    .store
                    .data_mut()
                    .effects
                    .push((suggestion, suggestion));
                trap.resume(&mut runtime.store, &[Val::I32(suggestion)], &mut output)?
            }
            ResumableCall::Finished => return Err("finished capture".into()),
        };
    } else {
        runtime.initialize(rom.as_deref())?;
        baseline = None;
        if mode == "stop-loop" {
            runtime.store.set_fuel(1000)?;
            runtime.store.data_mut().fuel_supplied = 1000;
        }
        runtime.store.data_mut().stop_import = mode == "stop-import";
        setup = start.elapsed();
        run_start = Instant::now();
        call = runtime.root.call_resumable(
            &mut runtime.store,
            &[Val::I32(if rom.is_some() { frames } else { 11 })],
            &mut output,
        )?;
    }
    let execution = run_start.elapsed();
    let capture_start = Instant::now();
    let suspended = !matches!(call, ResumableCall::Finished);
    let captured = runtime.capture(suspended.then_some(&call))?;
    let capture_time = capture_start.elapsed();
    let comparison_start = Instant::now();
    let pages = baseline
        .as_ref()
        .map(|memory| changed_pages(memory, &captured.memory));
    let comparison_time = comparison_start.elapsed();
    let bytes = postcard::to_stdvec(&captured)?;
    if mode != "resume" {
        fs::write(&args[2], &bytes)?;
    }
    let report = serde_json::json!({"module": digest(&module), "state_sha256": digest(&bytes), "suspended": suspended,
        "result": output[0].i32(), "fuel": captured.fuel, "pending": captured.host.pending, "effects": captured.host.effects,
        "globals": captured.globals, "frames": captured.frames, "live_registers": captured.values.len(),
        "memory_bytes": captured.memory.len(), "memory_prefix": captured.memory[..96.min(captured.memory.len())].to_vec(), "snapshot_bytes": bytes.len(), "setup_or_restore_seconds": setup.as_secs_f64(),
        "run_seconds": execution.as_secs_f64(), "capture_seconds": capture_time.as_secs_f64(), "exact_page_comparison_seconds": comparison_time.as_secs_f64(), "changed_pages_from_restore": pages, "host": [env::consts::OS, env::consts::ARCH]});
    fs::write(&args[3], serde_json::to_vec_pretty(&report)?)?;
    println!("{report}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<u8> {
        let mut source = include_str!("../../guest/continuation.wat").replace("10000", "32");
        let end = source.rfind(')').unwrap();
        source.insert_str(end, "(export \"__harmony_func_0\" (func $decision)) (export \"__harmony_func_1\" (func $other)) (export \"__harmony_func_2\" (func $nested)) (export \"__harmony_func_3\" (func $run)) (export \"__harmony_global_0\" (global $g)) (export \"__harmony_global_1\" (global $float)) (export \"__harmony_table_0\" (table $table))");
        wat::parse_str(source).unwrap()
    }

    fn resume(runtime: &mut Runtime, call: ResumableCall, answer: i32) -> i32 {
        let ResumableCall::HostTrap(trap) = call else {
            panic!("expected host import")
        };
        runtime.store.data_mut().stop_import = false;
        runtime.store.data_mut().pending = None;
        runtime.store.data_mut().effects.push((11, answer));
        let mut result = [Val::I32(0)];
        assert!(matches!(
            trap.resume(&mut runtime.store, &[Val::I32(answer)], &mut result)
                .unwrap(),
            ResumableCall::Finished
        ));
        result[0].i32().unwrap()
    }

    #[test]
    fn captures_are_inert_and_branches_preserve_live_state() {
        let module = fixture();
        let mut source = Runtime::new(&module).unwrap();
        source.initialize(None).unwrap();
        source.store.data_mut().stop_import = true;
        let call = source
            .root
            .call_resumable(&mut source.store, &[Val::I32(11)], &mut [Val::I32(0)])
            .unwrap();
        let mut captured = source.capture(Some(&call)).unwrap();
        let repeat = source.capture(Some(&call)).unwrap();
        assert_eq!(captured, repeat);
        let mut left = Runtime::new(&module).unwrap();
        let mut right = Runtime::new(&module).unwrap();
        let left_call = left.restore(&captured).unwrap();
        let right_call = right.restore(&captured).unwrap();
        assert_eq!(resume(&mut left, left_call, 13), 530);
        assert_eq!(resume(&mut right, right_call, 19), 536);
        assert_eq!(captured.host.pending, Some(11));
        let base = captured.frames.last().unwrap()[2] as usize;
        captured.values[base + 2] += 1;
        captured.globals.get_mut("__harmony_global_0").unwrap().1 += 5;
        captured.tables.get_mut("__harmony_table_0").unwrap()[1] = Some("__harmony_func_2".into());
        let mut planted = Runtime::new(&module).unwrap();
        let planted_call = planted.restore(&captured).unwrap();
        assert_eq!(resume(&mut planted, planted_call, 13), 536);
        let state = planted.capture(None).unwrap();
        assert_eq!(
            state.tables["__harmony_table_0"][1],
            Some("__harmony_func_2".into())
        );
    }

    #[test]
    fn fuel_stop_restores_inside_an_uninstrumented_loop() {
        let module = fixture();
        let mut source = Runtime::new(&module).unwrap();
        source.initialize(None).unwrap();
        source.store.set_fuel(40).unwrap();
        source.store.data_mut().fuel_supplied = 40;
        let call = source
            .root
            .call_resumable(&mut source.store, &[Val::I32(11)], &mut [Val::I32(0)])
            .unwrap();
        assert!(matches!(call, ResumableCall::OutOfFuel(_)));
        let captured = source.capture(Some(&call)).unwrap();
        let mut target = Runtime::new(&module).unwrap();
        let ResumableCall::OutOfFuel(trap) = target.restore(&captured).unwrap() else {
            panic!("fuel stop")
        };
        let consumed = 40 - captured.fuel;
        target.store.set_fuel(FUEL - consumed).unwrap();
        target.store.data_mut().fuel_supplied = FUEL;
        let mut output = [Val::I32(0)];
        assert!(matches!(
            trap.resume(&mut target.store, &mut output).unwrap(),
            ResumableCall::Finished
        ));
        let resumed = target.capture(None).unwrap();
        let mut uninterrupted = Runtime::new(&module).unwrap();
        uninterrupted.initialize(None).unwrap();
        uninterrupted
            .root
            .call(&mut uninterrupted.store, &[Val::I32(11)], &mut output)
            .unwrap();
        assert_eq!(resumed, uninterrupted.capture(None).unwrap());
    }

    #[test]
    fn dropped_segments_remain_dropped_after_fresh_restore() {
        let module = wat::parse_str("(module (import \"harmony_v1\" \"decision\" (func $decision (param i32) (result i32))) (memory (export \"memory\") 1 1) (data $data \"x\") (func (export \"run\") (param i32) (result i32) (data.drop $data) (drop (call $decision (i32.const 7))) (memory.init $data (i32.const 0) (i32.const 0) (i32.const 1)) (i32.const 1)))").unwrap();
        let mut source = Runtime::new(&module).unwrap();
        source.store.data_mut().stop_import = true;
        let call = source
            .root
            .call_resumable(&mut source.store, &[Val::I32(11)], &mut [Val::I32(0)])
            .unwrap();
        let captured = source.capture(Some(&call)).unwrap();
        assert_eq!(captured.data_segments, vec![0]);
        let mut target = Runtime::new(&module).unwrap();
        let ResumableCall::HostTrap(trap) = target.restore(&captured).unwrap() else {
            panic!("pending import")
        };
        assert!(
            trap.resume(&mut target.store, &[Val::I32(13)], &mut [Val::I32(0)])
                .is_err()
        );
    }

    #[test]
    fn exact_pages_observe_sparse_and_bulk_changes() {
        let before = vec![0; 4 * 4096];
        let mut after = before.clone();
        after[4095] = 1;
        after[2 * 4096..].fill(7);
        assert_eq!(changed_pages(&before, &after), vec![0, 2, 3]);
        assert!(changed_pages(&after, &after).is_empty());
    }

    #[test]
    fn trusted_continuation_restores_into_an_independent_engine() {
        let bytes = wat::parse_str("(module (import \"harmony_v1\" \"decision\" (func $decision (param i32) (result i32))) (memory (export \"memory\") 1 1) (func (export \"run\") (param i32) (result i32) (i32.add (local.get 0) (call $decision (i32.const 7)))))").unwrap();
        let mut source = Runtime::new(&bytes).unwrap();
        source.store.data_mut().stop_import = true;
        let call = source
            .root
            .call_resumable(&mut source.store, &[Val::I32(11)], &mut [Val::I32(0)])
            .unwrap();
        let captured = source.capture(Some(&call)).unwrap();
        let mut target = Runtime::new(&bytes).unwrap();
        let call = target.restore(&captured).unwrap();
        drop(source);
        drop(call.harmony_capture().unwrap());
        let ResumableCall::HostTrap(trap) = call else {
            panic!("expected pending import")
        };
        let mut result = [Val::I32(0)];
        assert!(matches!(
            trap.resume(&mut target.store, &[Val::I32(13)], &mut result)
                .unwrap(),
            ResumableCall::Finished
        ));
        assert_eq!(result[0].i32(), Some(24));
    }
}

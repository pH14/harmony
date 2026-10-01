// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{
    admission::{AdmissionError, AdmittedModule},
    services::{Host, Pending},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};
use wasmi::{
    Caller, Engine, Func, FuncRef, HarmonyContinuation, Instance, Linker, Memory, Module,
    ResumableCall, Store, Val,
};
pub(crate) type Result<T> = std::result::Result<T, AdmissionError>;
pub(crate) fn error(message: impl ToString) -> AdmissionError {
    AdmissionError(message.to_string())
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Scalar {
    I32(u32),
    I64(u64),
    F32(u32),
    F64(u64),
}
impl Scalar {
    fn from_value(value: Val) -> Result<Self> {
        Ok(match value {
            Val::I32(x) => Self::I32(x as u32),
            Val::I64(x) => Self::I64(x as u64),
            Val::F32(x) => Self::F32(x.to_bits()),
            Val::F64(x) => Self::F64(x.to_bits()),
            _ => return Err(error("reference value exceeds the scalar profile")),
        })
    }
    fn value(&self) -> Val {
        match *self {
            Self::I32(x) => Val::I32(x as i32),
            Self::I64(x) => Val::I64(x as i64),
            Self::F32(x) => Val::F32(wasmi::core::F32::from_bits(x)),
            Self::F64(x) => Val::F64(wasmi::core::F64::from_bits(x)),
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Invocation {
    pub name: String,
    pub arguments: Vec<Scalar>,
}
impl Invocation {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            arguments: Vec::new(),
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Continuation {
    pub(crate) values: Vec<u64>,
    pub(crate) frames: Vec<[u64; 5]>,
    pub(crate) required: Option<u64>,
    pub(crate) result: Option<i16>,
}
#[derive(Clone, Debug)]
pub(crate) struct Capture {
    pub(crate) memory: Vec<u8>,
    pub(crate) globals: BTreeMap<String, Scalar>,
    pub(crate) tables: BTreeMap<String, Vec<Option<u32>>>,
    pub(crate) fuel: u64,
    pub(crate) host: Host,
    pub(crate) continuation: Option<Continuation>,
    pub(crate) data: Vec<usize>,
    pub(crate) elements: Vec<u32>,
    pub(crate) invocation: Invocation,
    pub(crate) started: bool,
    pub(crate) finished: bool,
    pub(crate) trap: Option<String>,
    pub(crate) outputs: Vec<Scalar>,
}
pub(crate) struct Runtime {
    pub(crate) admitted: Arc<AdmittedModule>,
    pub(crate) engine: Engine,
    module: Module,
    pub(crate) instance: Instance,
    pub(crate) store: Store<Host>,
    pub(crate) memory: Memory,
    pub(crate) imports: BTreeMap<(String, String), Func>,
    pub(crate) root: Func,
    pub(crate) invocation: Invocation,
    pub(crate) call: Option<ResumableCall>,
    pub(crate) outputs: Vec<Val>,
    pub(crate) started: bool,
    pub(crate) finished: bool,
    pub(crate) trap: Option<String>,
}
fn pending(
    mut caller: Caller<'_, Host>,
    request: Pending,
) -> std::result::Result<i32, wasmi::Error> {
    if caller.data().pending.is_some() {
        return Err(wasmi::Error::new("duplicate pending request"));
    }
    caller.data_mut().pending = Some(request);
    Err(wasmi::Error::new("pending deterministic request"))
}
impl Runtime {
    pub(crate) fn new(
        admitted: Arc<AdmittedModule>,
        host: Host,
        invocation: Invocation,
    ) -> Result<Self> {
        if !admitted.entries.contains(&invocation.name) {
            return Err(error("entry must name an exported guest function"));
        }
        let engine = admitted.engine()?;
        let module = Module::new(&engine, admitted.bytes()).map_err(error)?;
        Self::instantiate(admitted, engine, module, host, invocation)
    }
    fn instantiate(
        admitted: Arc<AdmittedModule>,
        engine: Engine,
        module: Module,
        host: Host,
        invocation: Invocation,
    ) -> Result<Self> {
        if !admitted.entries.contains(&invocation.name) {
            return Err(error("entry must name an exported guest function"));
        }
        let mut store = Store::new(&engine, host);
        store.set_fuel(0).map_err(error)?;
        let mut linker = Linker::new(&engine);
        let mut imports = BTreeMap::new();
        for (module, name) in admitted.imports() {
            let function = match name.as_str() {
                "request" => Func::wrap(
                    &mut store,
                    |caller: Caller<'_, Host>,
                     op: i32,
                     pointer: i32,
                     len: i32,
                     output: i32,
                     capacity: i32| {
                        pending(
                            caller,
                            Pending::Request([
                                op as u32,
                                pointer as u32,
                                len as u32,
                                output as u32,
                                capacity as u32,
                            ]),
                        )
                    },
                ),
                "fd_write" => Func::wrap(
                    &mut store,
                    |caller: Caller<'_, Host>, fd: i32, iovecs: i32, count: i32, written: i32| {
                        pending(
                            caller,
                            Pending::Write([
                                fd as u32,
                                iovecs as u32,
                                count as u32,
                                written as u32,
                            ]),
                        )
                    },
                ),
                "fd_close" => Func::wrap(&mut store, |caller: Caller<'_, Host>, fd: i32| {
                    pending(caller, Pending::Close(fd as u32))
                }),
                "fd_seek" => Func::wrap(
                    &mut store,
                    |caller: Caller<'_, Host>, fd: i32, offset: i64, whence: i32, output: i32| {
                        pending(
                            caller,
                            Pending::Seek {
                                fd: fd as u32,
                                offset,
                                whence: whence as u32,
                                output: output as u32,
                            },
                        )
                    },
                ),
                _ => return Err(error("admitted import lacks transport")),
            };
            linker.define(module, name, function).map_err(error)?;
            imports.insert((module.clone(), name.clone()), function);
        }
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(error)?
            .ensure_no_start(&mut store)
            .map_err(error)?;
        let memory = instance
            .get_memory(&store, "__harmony_memory")
            .ok_or_else(|| error("missing linear memory"))?;
        let root = instance
            .get_func(&store, &invocation.name)
            .ok_or_else(|| error("missing entry function"))?;
        let ty = root.ty(&store);
        if ty.params().len() != invocation.arguments.len()
            || ty
                .params()
                .iter()
                .zip(&invocation.arguments)
                .any(|(ty, value)| *ty != value.value().ty())
        {
            return Err(error("entry arguments do not match the module"));
        }
        let outputs = ty.results().iter().copied().map(Val::default).collect();
        Ok(Self {
            admitted,
            engine,
            module,
            instance,
            store,
            memory,
            imports,
            root,
            invocation,
            call: None,
            outputs,
            started: false,
            finished: false,
            trap: None,
        })
    }
    pub(crate) fn execute(&mut self) -> Result<()> {
        if self.finished || self.trap.is_some() {
            return Ok(());
        }
        let result = if let Some(call) = self.call.take() {
            match call {
                ResumableCall::OutOfFuel(call) => call.resume(&mut self.store, &mut self.outputs),
                ResumableCall::HostTrap(call) => {
                    self.call = Some(ResumableCall::HostTrap(call));
                    return Err(error("pending import must complete before execution"));
                }
                ResumableCall::Finished => return Err(error("unexpected finished continuation")),
            }
        } else {
            if self.started {
                return Err(error("live execution lost its continuation"));
            }
            self.started = true;
            let arguments: Vec<_> = self
                .invocation
                .arguments
                .iter()
                .map(Scalar::value)
                .collect();
            self.root
                .call_resumable(&mut self.store, &arguments, &mut self.outputs)
        };
        match result {
            Ok(ResumableCall::Finished) => self.finished = true,
            Ok(call) => self.call = Some(call),
            Err(failure) if failure.as_trap_code().is_some() => {
                self.trap = Some(failure.to_string())
            }
            Err(failure) => {
                return Err(error(format!(
                    "interpreter infrastructure failure: {failure}"
                )));
            }
        }
        Ok(())
    }
    pub(crate) fn capture(&mut self) -> Result<Capture> {
        let functions: Vec<_> = self
            .instance
            .exports(&self.store)
            .filter_map(|export| {
                let index = export
                    .name()
                    .strip_prefix("__harmony_func_")?
                    .parse::<u32>()
                    .ok()?;
                Some((index, export.into_func()?))
            })
            .collect();
        let mut globals = BTreeMap::new();
        let mut tables = BTreeMap::new();
        for export in self.instance.exports(&self.store) {
            let name = export.name().to_owned();
            if name.starts_with("__harmony_global_") {
                let global = export
                    .into_global()
                    .ok_or_else(|| error("invalid global export"))?;
                if global.ty(&self.store).mutability().is_mut() {
                    globals.insert(name, Scalar::from_value(global.get(&self.store))?);
                }
            } else if name.starts_with("__harmony_table_") {
                let table = export
                    .into_table()
                    .ok_or_else(|| error("invalid table export"))?;
                let mut entries = Vec::new();
                for index in 0..table.size(&self.store) {
                    let value = table
                        .get(&self.store, index)
                        .ok_or_else(|| error("missing table entry"))?;
                    let reference = value
                        .funcref()
                        .ok_or_else(|| error("non-function table entry"))?;
                    entries.push(
                        reference
                            .func()
                            .map(|function| {
                                functions
                                    .iter()
                                    .find(|(_, candidate)| {
                                        wasmi::core::UntypedVal::from(FuncRef::from(*candidate))
                                            .to_bits64()
                                            == wasmi::core::UntypedVal::from(FuncRef::from(
                                                *function,
                                            ))
                                            .to_bits64()
                                    })
                                    .map(|(index, _)| *index)
                                    .ok_or_else(|| error("table function lacks stable identity"))
                            })
                            .transpose()?,
                    );
                }
                tables.insert(name, entries);
            }
        }
        let continuation = self
            .call
            .as_ref()
            .map(ResumableCall::harmony_capture)
            .transpose()
            .map_err(error)?
            .map(|capture| Continuation {
                values: capture.values,
                frames: capture.frames,
                required: capture.required_fuel,
                result: capture.caller_result,
            });
        let (data, elements) = self.instance.harmony_segment_lengths(&mut self.store);
        Ok(Capture {
            memory: self.memory.data(&self.store).to_vec(),
            globals,
            tables,
            fuel: self.store.get_fuel().map_err(error)?,
            host: self.store.data().clone(),
            continuation,
            data,
            elements,
            invocation: self.invocation.clone(),
            started: self.started,
            finished: self.finished,
            trap: self.trap.clone(),
            outputs: self
                .outputs
                .iter()
                .cloned()
                .map(Scalar::from_value)
                .collect::<Result<_>>()?,
        })
    }
    pub(crate) fn restore(admitted: Arc<AdmittedModule>, capture: &Capture) -> Result<Self> {
        let runtime = Self::new(admitted, capture.host.clone(), capture.invocation.clone())?;
        Self::finish_restore(runtime, capture)
    }
    pub(crate) fn restore_cached(&self, capture: &Capture) -> Result<Self> {
        let runtime = Self::instantiate(
            self.admitted.clone(),
            self.engine.clone(),
            self.module.clone(),
            capture.host.clone(),
            capture.invocation.clone(),
        )?;
        Self::finish_restore(runtime, capture)
    }
    fn finish_restore(mut runtime: Self, capture: &Capture) -> Result<Self> {
        if capture.memory.len() != runtime.memory.data(&runtime.store).len()
            || capture.outputs.len() != runtime.outputs.len()
            || capture
                .outputs
                .iter()
                .zip(&runtime.outputs)
                .any(|(captured, output)| captured.value().ty() != output.ty())
        {
            return Err(error("snapshot memory or root result shape differs"));
        }
        let expected_globals: std::collections::BTreeSet<_> = runtime
            .instance
            .exports(&runtime.store)
            .filter(|export| {
                export.name().starts_with("__harmony_global_")
                    && export
                        .clone()
                        .into_global()
                        .is_some_and(|global| global.ty(&runtime.store).mutability().is_mut())
            })
            .map(|export| export.name().to_owned())
            .collect();
        let expected_tables: std::collections::BTreeSet<_> = runtime
            .instance
            .exports(&runtime.store)
            .filter(|export| export.name().starts_with("__harmony_table_"))
            .map(|export| export.name().to_owned())
            .collect();
        if expected_globals != capture.globals.keys().cloned().collect()
            || expected_tables != capture.tables.keys().cloned().collect()
        {
            return Err(error("snapshot global or table inventory differs"));
        }
        runtime
            .memory
            .write(&mut runtime.store, 0, &capture.memory)
            .map_err(error)?;
        for (name, value) in &capture.globals {
            runtime
                .instance
                .get_global(&runtime.store, name)
                .ok_or_else(|| error("missing captured global"))?
                .set(&mut runtime.store, value.value())
                .map_err(error)?;
        }
        for (name, entries) in &capture.tables {
            let table = runtime
                .instance
                .get_table(&runtime.store, name)
                .ok_or_else(|| error("missing captured table"))?;
            if table.size(&runtime.store) != entries.len() as u64 {
                return Err(error("snapshot table capacity differs"));
            }
            for (index, entry) in entries.iter().enumerate() {
                let function = entry
                    .map(|index| {
                        runtime
                            .instance
                            .get_func(&runtime.store, &format!("__harmony_func_{index}"))
                            .ok_or_else(|| error("missing captured function"))
                    })
                    .transpose()?;
                table
                    .set(
                        &mut runtime.store,
                        index as u64,
                        Val::FuncRef(FuncRef::new(function)),
                    )
                    .map_err(error)?;
            }
        }
        runtime
            .instance
            .harmony_restore_segments(&mut runtime.store, &capture.data, &capture.elements)
            .map_err(error)?;
        runtime.store.set_fuel(capture.fuel).map_err(error)?;
        if let Some(continuation) = &capture.continuation {
            let host = if let Some(pending) = &capture.host.pending {
                let (module, name) = pending.import();
                *runtime
                    .imports
                    .get(&(module.into(), name.into()))
                    .ok_or_else(|| error("pending import is absent"))?
            } else {
                runtime.root
            };
            let snapshot = HarmonyContinuation {
                values: continuation.values.clone(),
                frames: continuation.frames.clone(),
                required_fuel: continuation.required,
                caller_result: continuation.result,
            };
            ResumableCall::harmony_validate(
                &runtime.engine,
                &runtime.store,
                runtime.instance,
                runtime.root,
                host,
                &snapshot,
            )
            .map_err(error)?;
            // SAFETY: Admission excludes reference-valued registers and multiple instances. The validator checks compiled positions, contiguous frame allocations, immutable constants and parent/pending result ranges against this exact eagerly compiled module before reconstructing its continuation.
            runtime.call = Some(unsafe {
                ResumableCall::harmony_restore(
                    runtime.engine.clone(),
                    runtime.instance,
                    runtime.root,
                    host,
                    &snapshot,
                )
                .map_err(error)?
            });
        }
        runtime.started = capture.started;
        runtime.finished = capture.finished;
        runtime.trap = capture.trap.clone();
        runtime.outputs = capture.outputs.iter().map(Scalar::value).collect();
        Ok(runtime)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admission::Profile;
    use environment::input_spec::{InputSpec, nominal_factory};
    #[test]
    fn continuation_validation_rejects_malformed_frames_and_operand_positions() {
        let source = wat::parse_str(
            r#"(module
          (type $t (func (param i64) (result i64)))
          (import "harmony_v1" "request" (func $request (param i32 i32 i32 i32 i32) (result i32)))
          (memory 1 1) (table 1 1 funcref) (elem (i32.const 0) $leaf)
          (func $leaf (type $t) local.get 0 i64.const 1234605616436508552 i64.add
           i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0 call $request drop)
          (func (export "run") (result i64) i64.const 9001 i32.const 0 call_indirect (type $t)))"#,
        )
        .unwrap();
        let module = Arc::new(
            AdmittedModule::new(
                &source,
                Profile {
                    memory_pages: 1,
                    ..Profile::default()
                },
            )
            .unwrap(),
        );
        let env = InputSpec::seeded(4)
            .materialize(&nominal_factory())
            .unwrap();
        let mut runtime =
            Runtime::new(module.clone(), Host::new(env), Invocation::new("run")).unwrap();
        runtime.store.set_fuel(10000).unwrap();
        runtime.execute().unwrap();
        let captured = runtime.capture().unwrap();
        let continuation = runtime.call.as_ref().unwrap().harmony_capture().unwrap();
        assert_eq!(continuation.frames.len(), 2);
        let host = runtime.imports[&("harmony_v1".into(), "request".into())];
        let validate = |snapshot: &HarmonyContinuation| {
            ResumableCall::harmony_validate(
                &runtime.engine,
                &runtime.store,
                runtime.instance,
                runtime.root,
                host,
                snapshot,
            )
        };
        validate(&continuation).unwrap();
        let mut bad = continuation.clone();
        bad.values.pop();
        assert!(validate(&bad).is_err());
        for (column, value) in [
            (0, u64::MAX),
            (1, u64::MAX),
            (2, u64::MAX),
            (3, u64::MAX),
            (4, u64::MAX),
        ] {
            let mut bad = continuation.clone();
            bad.frames.last_mut().unwrap()[column] = value;
            assert!(validate(&bad).is_err());
        }
        let mut bad = continuation.clone();
        bad.frames[0][4] = 1;
        assert!(validate(&bad).is_err());
        let mut bad = continuation.clone();
        bad.caller_result = Some(-1);
        assert!(validate(&bad).is_err());
        let mut bad = continuation.clone();
        bad.required_fuel = Some(1);
        assert!(validate(&bad).is_err());
        let constant = continuation
            .frames
            .iter()
            .find(|frame| frame[2] > frame[3])
            .expect("large literal needs an immutable constant register")[3]
            as usize;
        let mut bad = continuation.clone();
        bad.values[constant] ^= 1;
        assert!(validate(&bad).is_err());
        let mut operands = 0;
        for offset in 0..continuation.frames.last().unwrap()[1] {
            let mut bad = continuation.clone();
            bad.frames.last_mut().unwrap()[1] = offset;
            operands += usize::from(validate(&bad).is_err());
        }
        assert!(operands > 0);
        let mut restored = Runtime::restore(module, &captured).unwrap();
        restored
            .call
            .as_mut()
            .unwrap()
            .harmony_complete_i32(&restored.store, -2)
            .unwrap();
        restored.store.data_mut().pending = None;
        restored.execute().unwrap();
        assert!(restored.finished);
        assert_eq!(
            Scalar::from_value(restored.outputs[0].clone()).unwrap(),
            Scalar::I64(1234605616436508552 + 9001)
        );
    }
}

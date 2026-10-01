// SPDX-License-Identifier: AGPL-3.0-or-later
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use wasm_encoder::{
    Instruction,
    reencode::{self, Reencode},
};
use wasmparser::{Operator, Parser, Payload, TypeRef, ValType};

#[derive(Debug, thiserror::Error)]
#[error("WASM admission: {0}")]
pub struct AdmissionError(pub String);
type Result<T> = std::result::Result<T, AdmissionError>;
fn reject(message: impl Into<String>) -> AdmissionError {
    AdmissionError(message.into())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub memory_pages: u32,
    pub maximum_table_elements: u32,
    pub maximum_function_bytes: u32,
    pub maximum_module_bytes: u32,
    pub stack_registers: u32,
    pub recursion_depth: u32,
    pub fuel_quantum: u64,
}
impl Default for Profile {
    fn default() -> Self {
        Self {
            memory_pages: 256,
            maximum_table_elements: 4096,
            maximum_function_bytes: 65536,
            maximum_module_bytes: 4 * 1024 * 1024,
            stack_registers: 131072,
            recursion_depth: 256,
            fuel_quantum: 1024,
        }
    }
}
impl Profile {
    pub fn validate(&self) -> Result<()> {
        if self.memory_pages == 0
            || self.memory_pages > 256
            || self.maximum_table_elements > 16384
            || self.maximum_function_bytes == 0
            || self.maximum_function_bytes > 65536
            || self.maximum_module_bytes == 0
            || self.maximum_module_bytes > 16 * 1024 * 1024
            || self.stack_registers < 1024
            || self.stack_registers > 131072
            || self.recursion_depth == 0
            || self.recursion_depth > 1024
            || self.fuel_quantum != 1024
        {
            return Err(reject("limits exceed the qualified profile"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub struct AdmittedModule {
    pub(crate) bytes: Vec<u8>,
    pub(crate) source_digest: [u8; 32],
    pub(crate) execution_digest: [u8; 32],
    pub(crate) profile: Profile,
    pub(crate) imports: Vec<(String, String)>,
    pub(crate) entries: std::collections::BTreeSet<String>,
}
fn scalar(ty: ValType) -> bool {
    matches!(
        ty,
        ValType::I32 | ValType::I64 | ValType::F32 | ValType::F64
    )
}
fn allowed(op: &Operator<'_>) -> bool {
    if matches!(op, Operator::MemoryGrow { .. }) {
        return false;
    }
    macro_rules! proposal {
        (@mvp) => {
            true
        };
        (@sign_extension) => {
            true
        };
        (@saturating_float_to_int) => {
            true
        };
        (@bulk_memory) => {
            true
        };
        (@$other:ident) => {
            false
        };
    }
    macro_rules! instructions {
        ($( @$proposal:ident $op:ident $({ $($arg:ident: $argty:ty),* })? => $visit:ident ($($ann:tt)*))*) => {
            match op { $( Operator::$op $({ $($arg: _),* })? => proposal!(@$proposal), )* _ => false }
        };
    }
    wasmparser::for_each_operator!(instructions)
}
fn import_signature(module: &str, name: &str) -> Option<(Vec<ValType>, Vec<ValType>)> {
    use ValType::{I32, I64};
    let params = match (module, name) {
        ("harmony_v1", "request") => vec![I32, I32, I32, I32, I32],
        ("wasi_snapshot_preview1", "fd_write") => vec![I32, I32, I32, I32],
        ("wasi_snapshot_preview1", "fd_close") => vec![I32],
        ("wasi_snapshot_preview1", "fd_seek") => vec![I32, I64, I32, I32],
        _ => return None,
    };
    Some((params, vec![I32]))
}
impl AdmittedModule {
    pub fn new(source: &[u8], profile: Profile) -> Result<Self> {
        profile.validate()?;
        if source.len() > profile.maximum_module_bytes as usize {
            return Err(reject("module exceeds its byte limit"));
        }
        let features = wasmparser::WasmFeatures::WASM1
            | wasmparser::WasmFeatures::BULK_MEMORY
            | wasmparser::WasmFeatures::SIGN_EXTENSION
            | wasmparser::WasmFeatures::SATURATING_FLOAT_TO_INT
            | wasmparser::WasmFeatures::REFERENCE_TYPES;
        wasmparser::Validator::new_with_features(features)
            .validate_all(source)
            .map_err(|e| reject(e.to_string()))?;
        let mut types = Vec::new();
        let mut imports = Vec::new();
        let mut entries = std::collections::BTreeSet::new();
        let mut functions = 0;
        let mut globals = 0;
        let mut tables = 0;
        let mut memories = 0;
        let mut has_exports = false;
        for item in Parser::new(0).parse_all(source) {
            match item.map_err(|e| reject(e.to_string()))? {
                Payload::TypeSection(section) => {
                    for ty in section.into_iter_err_on_gc_types() {
                        let ty = ty.map_err(|e| reject(e.to_string()))?;
                        if !ty.params().iter().chain(ty.results()).copied().all(scalar)
                            || ty.results().len() > 1
                            || ty.params().len() > 64
                        {
                            return Err(reject(
                                "only bounded scalar function signatures are admitted",
                            ));
                        }
                        types.push(ty);
                    }
                }
                Payload::ImportSection(section) => {
                    for import in section {
                        let import = import.map_err(|e| reject(e.to_string()))?;
                        let TypeRef::Func(index) = import.ty else {
                            return Err(reject("only function imports are admitted"));
                        };
                        let (params, results) = import_signature(import.module, import.name)
                            .ok_or_else(|| {
                                reject(format!(
                                    "unsupported import {}.{}",
                                    import.module, import.name
                                ))
                            })?;
                        let ty = &types[index as usize];
                        if ty.params() != params || ty.results() != results {
                            return Err(reject("import signature differs from the versioned ABI"));
                        }
                        if imports
                            .iter()
                            .any(|(module, name)| module == import.module && name == import.name)
                        {
                            return Err(reject("duplicate closed ABI import"));
                        }
                        imports.push((import.module.into(), import.name.into()));
                        functions += 1;
                    }
                }
                Payload::FunctionSection(section) => functions += section.count(),
                Payload::MemorySection(section) => {
                    for memory in section {
                        let memory = memory.map_err(|e| reject(e.to_string()))?;
                        memories += 1;
                        if memory.memory64
                            || memory.shared
                            || memory.page_size_log2.is_some()
                            || memory.initial != u64::from(profile.memory_pages)
                            || memory.maximum != Some(memory.initial)
                        {
                            return Err(reject("one fixed wasm32 memory is required"));
                        }
                    }
                }
                Payload::TableSection(section) => {
                    for table in section {
                        let table = table.map_err(|e| reject(e.to_string()))?;
                        tables += 1;
                        if table.ty.element_type != wasmparser::RefType::FUNCREF
                            || table.ty.table64
                            || table.ty.shared
                            || table.ty.maximum != Some(table.ty.initial)
                            || table.ty.initial > u64::from(profile.maximum_table_elements)
                        {
                            return Err(reject("only fixed function-index tables are admitted"));
                        }
                    }
                }
                Payload::GlobalSection(section) => {
                    for global in section {
                        let global = global.map_err(|e| reject(e.to_string()))?;
                        globals += 1;
                        if !scalar(global.ty.content_type) || global.ty.shared {
                            return Err(reject(
                                "reference-valued or shared globals are unsupported",
                            ));
                        }
                    }
                }
                Payload::ExportSection(section) => {
                    has_exports = true;
                    for export in section {
                        let export = export.map_err(|e| reject(e.to_string()))?;
                        if export.name.starts_with("__harmony_") {
                            return Err(reject("reserved execution export name"));
                        }
                        if export.kind == wasmparser::ExternalKind::Func
                            && export.index >= imports.len() as u32
                        {
                            entries.insert(export.name.to_owned());
                        }
                    }
                }
                Payload::StartSection { .. } => {
                    return Err(reject("automatic start is unsupported"));
                }
                Payload::CodeSectionEntry(body) => {
                    if body.range().len() > profile.maximum_function_bytes as usize {
                        return Err(reject("function body exceeds its byte limit"));
                    }
                    let mut locals = 0u64;
                    for local in body
                        .get_locals_reader()
                        .map_err(|e| reject(e.to_string()))?
                    {
                        let (count, ty) = local.map_err(|e| reject(e.to_string()))?;
                        locals += u64::from(count);
                        if !scalar(ty) || locals > u64::from(profile.stack_registers) {
                            return Err(reject("reference or excessive locals are unsupported"));
                        }
                    }
                    let mut reader = body
                        .get_operators_reader()
                        .map_err(|e| reject(e.to_string()))?;
                    while !reader.eof() {
                        let op = reader.read().map_err(|e| reject(e.to_string()))?;
                        if !allowed(&op) {
                            return Err(reject(format!("unsupported instruction {op:?}")));
                        }
                    }
                }
                Payload::Version {
                    encoding: wasmparser::Encoding::Module,
                    ..
                }
                | Payload::End(_)
                | Payload::DataSection(_)
                | Payload::ElementSection(_)
                | Payload::DataCountSection { .. }
                | Payload::CodeSectionStart { .. }
                | Payload::CustomSection(_) => {}
                _ => return Err(reject("unsupported section")),
            }
        }
        if !has_exports
            || functions <= imports.len() as u32
            || memories != 1
            || tables > 1
            || functions > 8192
            || globals > 4096
            || types.len() > 8192
        {
            return Err(reject("module resource count exceeds the profile"));
        }
        let source_digest = Sha256::digest(source).into();
        let mut transform = Transform {
            types: types.len() as u32,
            functions,
            globals,
            tables,
        };
        let mut module = wasm_encoder::Module::new();
        transform
            .parse_core_module(&mut module, Parser::new(0), source)
            .map_err(|e| reject(e.to_string()))?;
        let bytes = module.finish();
        wasmparser::Validator::new_with_features(features)
            .validate_all(&bytes)
            .map_err(|e| reject(e.to_string()))?;
        let mut identity = Sha256::new();
        identity.update(b"harmony-wasm-execution-v1;wasmi=.46.0;rust=1.97.0;encoder=.236.1;eager;scalar;nan-v1;abi-v1;fuel=wasmi-default;imports=64+bytes;time=fuel;quantum=1024");
        identity.update(Sha256::digest(include_bytes!(
            "../runtime/wasmi-0.46.0.crate"
        )));
        identity.update(Sha256::digest(include_bytes!(
            "../qualification/wasmi-snapshot.patch"
        )));
        identity.update(Sha256::digest(include_bytes!("admission.rs")));
        identity.update(Sha256::digest(include_bytes!("meter.rs")));
        identity.update(Sha256::digest(include_bytes!("../runtime/numerical.patch")));
        identity.update(Sha256::digest(include_bytes!(
            "../runtime/src/harmony_wasm.rs"
        )));
        identity.update(Sha256::digest(include_bytes!(
            "../runtime/import-completion.patch"
        )));
        identity.update(Sha256::digest(include_bytes!("services.rs")));
        identity.update(Sha256::digest(include_bytes!("runtime.rs")));
        identity.update(Sha256::digest(include_bytes!("session.rs")));
        identity.update(Sha256::digest(include_bytes!(
            "../runtime/validation.patch"
        )));
        identity.update(Sha256::digest(include_bytes!("artifact.rs")));
        for source in [
            include_bytes!("../../environment/src/channel.rs").as_slice(),
            include_bytes!("../../environment/src/sdk.rs").as_slice(),
            include_bytes!("../../environment/src/input_spec.rs").as_slice(),
            include_bytes!("../../hypercall-proto/src/lib.rs").as_slice(),
            include_bytes!("../../hypercall-proto/src/observation.rs").as_slice(),
        ] {
            identity.update(Sha256::digest(source));
        }
        identity.update(wasmi::HARMONY_COMPILER.as_bytes());
        identity.update(Sha256::digest(include_bytes!("../Cargo.lock")));
        identity.update(source_digest);
        identity.update(Sha256::digest(&bytes));
        identity.update(postcard::to_allocvec(&profile).map_err(|e| reject(e.to_string()))?);
        Ok(Self {
            bytes,
            source_digest,
            execution_digest: identity.finalize().into(),
            profile,
            imports,
            entries,
        })
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn source_digest(&self) -> [u8; 32] {
        self.source_digest
    }
    pub fn execution_digest(&self) -> [u8; 32] {
        self.execution_digest
    }
    pub fn profile(&self) -> &Profile {
        &self.profile
    }
    pub fn imports(&self) -> &[(String, String)] {
        &self.imports
    }
    pub fn identity_with_input(&self, input: &[u8]) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(self.execution_digest);
        hash.update((input.len() as u64).to_le_bytes());
        hash.update(input);
        hash.finalize().into()
    }
    pub fn engine(&self) -> Result<wasmi::Engine> {
        let mut config = wasmi::Config::default();
        config.consume_fuel(true);
        config.compilation_mode(wasmi::CompilationMode::Eager);
        config.set_stack_limits(
            wasmi::StackLimits::new(
                1024,
                self.profile.stack_registers as usize,
                self.profile.recursion_depth as usize,
            )
            .map_err(|e| reject(e.to_string()))?,
        );
        Ok(wasmi::Engine::new(&config))
    }
}

struct Transform {
    types: u32,
    functions: u32,
    globals: u32,
    tables: u32,
}
impl Reencode for Transform {
    type Error = std::convert::Infallible;
    fn parse_type_section(
        &mut self,
        section: &mut wasm_encoder::TypeSection,
        reader: wasmparser::TypeSectionReader<'_>,
    ) -> std::result::Result<(), reencode::Error<Self::Error>> {
        reencode::utils::parse_type_section(self, section, reader)?;
        section
            .ty()
            .function([wasm_encoder::ValType::F32], [wasm_encoder::ValType::F32]);
        section
            .ty()
            .function([wasm_encoder::ValType::F64], [wasm_encoder::ValType::F64]);
        Ok(())
    }
    fn parse_function_section(
        &mut self,
        section: &mut wasm_encoder::FunctionSection,
        reader: wasmparser::FunctionSectionReader<'_>,
    ) -> std::result::Result<(), reencode::Error<Self::Error>> {
        reencode::utils::parse_function_section(self, section, reader)?;
        section.function(self.types);
        section.function(self.types + 1);
        Ok(())
    }
    fn parse_export_section(
        &mut self,
        section: &mut wasm_encoder::ExportSection,
        reader: wasmparser::ExportSectionReader<'_>,
    ) -> std::result::Result<(), reencode::Error<Self::Error>> {
        reencode::utils::parse_export_section(self, section, reader)?;
        for index in 0..self.functions + 2 {
            section.export(
                &format!("__harmony_func_{index}"),
                wasm_encoder::ExportKind::Func,
                index,
            );
        }
        for index in 0..self.globals {
            section.export(
                &format!("__harmony_global_{index}"),
                wasm_encoder::ExportKind::Global,
                index,
            );
        }
        for index in 0..self.tables {
            section.export(
                &format!("__harmony_table_{index}"),
                wasm_encoder::ExportKind::Table,
                index,
            );
        }
        section.export("__harmony_memory", wasm_encoder::ExportKind::Memory, 0);
        Ok(())
    }
    fn parse_function_body(
        &mut self,
        code: &mut wasm_encoder::CodeSection,
        body: wasmparser::FunctionBody<'_>,
    ) -> std::result::Result<(), reencode::Error<Self::Error>> {
        let mut function = self.new_function_with_parsed_locals(&body)?;
        let mut reader = body.get_operators_reader()?;
        while !reader.eof() {
            let op = reader.read()?;
            let after32 = matches!(
                op,
                Operator::F32Add
                    | Operator::F32Sub
                    | Operator::F32Mul
                    | Operator::F32Div
                    | Operator::F32Min
                    | Operator::F32Max
                    | Operator::F32Sqrt
                    | Operator::F32Ceil
                    | Operator::F32Floor
                    | Operator::F32Trunc
                    | Operator::F32Nearest
                    | Operator::F32DemoteF64
            );
            let after64 = matches!(
                op,
                Operator::F64Add
                    | Operator::F64Sub
                    | Operator::F64Mul
                    | Operator::F64Div
                    | Operator::F64Min
                    | Operator::F64Max
                    | Operator::F64Sqrt
                    | Operator::F64Ceil
                    | Operator::F64Floor
                    | Operator::F64Trunc
                    | Operator::F64Nearest
                    | Operator::F64PromoteF32
            );
            if matches!(op, Operator::F32DemoteF64) {
                function.instruction(&Instruction::Call(self.functions + 1));
            }
            if matches!(op, Operator::F64PromoteF32) {
                function.instruction(&Instruction::Call(self.functions));
            }
            function.instruction(&self.instruction(op)?);
            if after32 {
                function.instruction(&Instruction::Call(self.functions));
            }
            if after64 {
                function.instruction(&Instruction::Call(self.functions + 1));
            }
        }
        code.function(&function);
        Ok(())
    }
    fn parse_code_section(
        &mut self,
        code: &mut wasm_encoder::CodeSection,
        reader: wasmparser::CodeSectionReader<'_>,
    ) -> std::result::Result<(), reencode::Error<Self::Error>> {
        reencode::utils::parse_code_section(self, code, reader)?;
        for wide in [false, true] {
            let ty = if wide {
                wasm_encoder::ValType::F64
            } else {
                wasm_encoder::ValType::F32
            };
            let mut f = wasm_encoder::Function::new([]);
            f.instruction(&Instruction::LocalGet(0))
                .instruction(&Instruction::LocalGet(0));
            f.instruction(&if wide {
                Instruction::F64Ne
            } else {
                Instruction::F32Ne
            });
            f.instruction(&Instruction::If(wasm_encoder::BlockType::Result(ty)));
            f.instruction(&if wide {
                Instruction::F64Const(f64::from_bits(0x7ff8000000000000).into())
            } else {
                Instruction::F32Const(f32::from_bits(0x7fc00000).into())
            });
            f.instruction(&Instruction::Else)
                .instruction(&Instruction::LocalGet(0))
                .instruction(&Instruction::End)
                .instruction(&Instruction::End);
            code.function(&f);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn profile() -> Profile {
        Profile {
            memory_pages: 4,
            ..Profile::default()
        }
    }
    fn admit(wat: &str) -> Result<AdmittedModule> {
        AdmittedModule::new(&wat::parse_str(wat).unwrap(), profile())
    }
    fn run(module: &AdmittedModule) -> (i32, Vec<u8>) {
        let engine = module.engine().unwrap();
        let compiled = wasmi::Module::new(&engine, &module.bytes).unwrap();
        let mut store = wasmi::Store::new(&engine, ());
        store.set_fuel(1_000_000).unwrap();
        let instance = wasmi::Linker::new(&engine)
            .instantiate(&mut store, &compiled)
            .unwrap()
            .ensure_no_start(&mut store)
            .unwrap();
        let result = instance
            .get_typed_func::<(), i32>(&store, "run")
            .unwrap()
            .call(&mut store, ())
            .unwrap();
        let memory = instance
            .get_memory(&store, "__harmony_memory")
            .unwrap()
            .data(&store)
            .to_vec();
        (result, memory)
    }
    #[test]
    fn rejects_reference_bodies_dynamic_resources_and_unknown_imports() {
        for body in [
            "(drop (ref.null func))",
            "(drop (memory.grow (i32.const 1)))",
            "(drop (v128.const i32x4 0 0 0 0))",
        ] {
            assert!(
                admit(&format!(
                    "(module (memory 4 4) (func (export \"run\") {body}))"
                ))
                .is_err()
            );
        }
        for module in [
            "(module (memory 4) (func (export \"run\")))",
            "(module (memory 4 4) (global funcref (ref.null func)) (func (export \"run\")))",
            "(module (memory 4 4) (func (export \"run\") (local externref)))",
            "(module (import \"unknown\" \"request\" (func)) (memory 4 4) (func (export \"run\")))",
            "(module (memory 4 4) (func $start) (start $start) (func (export \"run\")))",
            "(module (memory 4 4) (table 2 3 funcref) (func (export \"run\")))",
            "(module (memory 4 4) (func (export \"__harmony_func_0\")))",
        ] {
            assert!(admit(module).is_err(), "{module}");
        }
    }
    #[test]
    fn conversion_nan_signed_zero_and_subnormal_bits_are_explicit() {
        let source = "(module (memory 4 4) (func (export \"run\") (result i32)
        (f32.store (i32.const 0) (f32.demote_f64 (f64.const nan:0x123)))
        (f64.store (i32.const 8) (f64.promote_f32 (f32.const nan:0x123)))
        (f32.store (i32.const 16) (f32.mul (f32.const 0x1p-126) (f32.const 0.5)))
        (f64.store (i32.const 24) (f64.min (f64.const 0) (f64.const -0)))
        (i32.const 7)))";
        let module = admit(source).unwrap();
        let (result, memory) = run(&module);
        assert_eq!(result, 7);
        assert_eq!(
            u32::from_le_bytes(memory[0..4].try_into().unwrap()),
            0x7fc00000
        );
        assert_eq!(
            u64::from_le_bytes(memory[8..16].try_into().unwrap()),
            0x7ff8000000000000
        );
        assert_eq!(
            u32::from_le_bytes(memory[16..20].try_into().unwrap()),
            0x00400000
        );
        assert_eq!(
            u64::from_le_bytes(memory[24..32].try_into().unwrap()),
            0x8000000000000000
        );
        let repeated = admit(source).unwrap();
        assert_eq!(module.bytes, repeated.bytes);
        assert_eq!(module.execution_digest, repeated.execution_digest);
        assert_ne!(
            module.identity_with_input(b"one"),
            module.identity_with_input(b"two")
        );
    }
    #[test]
    fn indirect_calls_bulk_memory_and_passive_segments_are_admitted() {
        let module=admit("(module (memory 4 4) (table 1 1 funcref) (elem (i32.const 0) $callee) (data $data \"abc\") (func $callee (result i32) (i32.const 7)) (func (export \"run\") (result i32) (memory.init $data (i32.const 1) (i32.const 0) (i32.const 3)) (data.drop $data) (call_indirect (result i32) (i32.const 0))))").unwrap();
        let (result, memory) = run(&module);
        assert_eq!(result, 7);
        assert_eq!(&memory[1..4], b"abc");
    }
    #[test]
    fn arithmetic_nan_is_canonical_before_a_pending_import() {
        let mut config = wasmi::Config::default();
        config.consume_fuel(true);
        let engine = wasmi::Engine::new(&config);
        let source=wat::parse_str("(module (import \"harmony_v1\" \"request\" (func $request (param i32 i32 i32 i32 i32) (result i32))) (func (export \"run\") (param f64 f64) (result i64) (local $result f64) (local.set $result (f64.add (local.get 0) (local.get 1))) (drop (call $request (i32.const 0) (i32.wrap_i64 (i64.reinterpret_f64 (local.get $result))) (i32.wrap_i64 (i64.shr_u (i64.reinterpret_f64 (local.get $result)) (i64.const 32))) (i32.const 0) (i32.const 0))) (i64.reinterpret_f64 (local.get $result))))").unwrap();
        let module = wasmi::Module::new(&engine, &source).unwrap();
        let mut store = wasmi::Store::new(&engine, 0u64);
        store.set_fuel(1_000_000).unwrap();
        let mut linker = wasmi::Linker::new(&engine);
        linker
            .func_wrap(
                "harmony_v1",
                "request",
                |mut caller: wasmi::Caller<'_, u64>,
                 _: i32,
                 lo: i32,
                 hi: i32,
                 _: i32,
                 _: i32|
                 -> std::result::Result<i32, wasmi::Error> {
                    *caller.data_mut() = u64::from(lo as u32) | (u64::from(hi as u32) << 32);
                    Err(wasmi::Error::new("pending"))
                },
            )
            .unwrap();
        let instance = linker
            .instantiate(&mut store, &module)
            .unwrap()
            .ensure_no_start(&mut store)
            .unwrap();
        let root = instance.get_func(&store, "run").unwrap();
        let mut output = [wasmi::Val::I64(0)];
        let call = root
            .call_resumable(
                &mut store,
                &[
                    wasmi::Val::F64(wasmi::core::F64::from_bits(0xfff0000000000123)),
                    wasmi::Val::F64(wasmi::core::F64::from_bits(0)),
                ],
                &mut output,
            )
            .unwrap();
        assert!(matches!(call, wasmi::ResumableCall::HostTrap(_)));
        assert_eq!(*store.data(), 0x7ff8000000000000);
        assert!(
            call.harmony_capture()
                .unwrap()
                .values
                .contains(&0x7ff8000000000000)
        );
    }

    #[test]
    fn guest_recursion_and_memory_faults_are_deterministic_traps() {
        for (source, expected) in [
            (
                "(module (memory 4 4) (func $f (export \"run\") (result i32) (call $f)))",
                wasmi::core::TrapCode::StackOverflow,
            ),
            (
                "(module (memory 4 4) (func (export \"run\") (result i32) (i32.load (i32.const 262143))))",
                wasmi::core::TrapCode::MemoryOutOfBounds,
            ),
            (
                "(module (memory 4 4) (func (export \"run\") (result i32) (i32.div_s (i32.const 1) (i32.const 0))))",
                wasmi::core::TrapCode::IntegerDivisionByZero,
            ),
        ] {
            let module = admit(source).unwrap();
            let engine = module.engine().unwrap();
            let compiled = wasmi::Module::new(&engine, module.bytes()).unwrap();
            let mut store = wasmi::Store::new(&engine, ());
            store.set_fuel(1_000_000).unwrap();
            let instance = wasmi::Linker::new(&engine)
                .instantiate(&mut store, &compiled)
                .unwrap()
                .ensure_no_start(&mut store)
                .unwrap();
            let error = instance
                .get_typed_func::<(), i32>(&store, "run")
                .unwrap()
                .call(&mut store, ())
                .unwrap_err();
            assert_eq!(error.as_trap_code(), Some(expected));
        }
    }

    #[test]
    fn sdk_and_wasi_signatures_are_closed() {
        assert!(admit("(module (import \"harmony_v1\" \"request\" (func (param i32 i32 i32 i32 i32) (result i32))) (memory 4 4) (func (export \"run\")))").is_ok());
        assert!(admit("(module (import \"harmony_v1\" \"request\" (func (param i32) (result i32))) (memory 4 4) (func (export \"run\")))").is_err());
        assert!(admit("(module (import \"wasi_snapshot_preview1\" \"random_get\" (func (param i32 i32) (result i32))) (memory 4 4) (func (export \"run\")))").is_err());
    }
    #[test]
    fn partitioned_deadlines_preserve_fixed_metering_boundaries() {
        let module=admit("(module (memory 4 4) (func (export \"run\") (result i32) (local $i i32) (loop $again (local.set $i (i32.add (local.get $i) (i32.const 1))) (br_if $again (i32.lt_u (local.get $i) (i32.const 100000)))) (local.get $i)))").unwrap();
        type ExecutionTrace = (Vec<Vec<u64>>, Vec<Vec<[u64; 5]>>, i32);
        fn execute(module: &AdmittedModule, deadlines: &[u64]) -> ExecutionTrace {
            let engine = module.engine().unwrap();
            let compiled = wasmi::Module::new(&engine, &module.bytes).unwrap();
            let mut store = wasmi::Store::new(&engine, ());
            store.set_fuel(0).unwrap();
            let instance = wasmi::Linker::new(&engine)
                .instantiate(&mut store, &compiled)
                .unwrap()
                .ensure_no_start(&mut store)
                .unwrap();
            let root = instance.get_func(&store, "run").unwrap();
            let mut output = [wasmi::Val::I32(0)];
            let mut call = root.call_resumable(&mut store, &[], &mut output).unwrap();
            let mut meter = crate::meter::Meter::default();
            let mut values = Vec::new();
            let mut frames = Vec::new();
            for deadline in deadlines {
                let goal = crate::meter::Meter::rounded_deadline(*deadline).unwrap();
                while meter.moment(store.get_fuel().unwrap()).unwrap() < goal {
                    let remaining = store.get_fuel().unwrap();
                    store.set_fuel(meter.grant(remaining).unwrap()).unwrap();
                    call = match call {
                        wasmi::ResumableCall::OutOfFuel(stop) => {
                            stop.resume(&mut store, &mut output).unwrap()
                        }
                        _ => panic!("expected fuel boundary"),
                    };
                }
                let state = call.harmony_capture().unwrap();
                values.push(state.values);
                frames.push(state.frames);
            }
            loop {
                match call {
                    wasmi::ResumableCall::OutOfFuel(stop) => {
                        let remaining = store.get_fuel().unwrap();
                        store.set_fuel(meter.grant(remaining).unwrap()).unwrap();
                        call = stop.resume(&mut store, &mut output).unwrap();
                    }
                    wasmi::ResumableCall::Finished => break,
                    _ => panic!("unexpected import"),
                }
            }
            (values, frames, output[0].i32().unwrap())
        }
        let many = execute(&module, &[1, 1025, 4097]);
        let one = execute(&module, &[4097]);
        assert_eq!(many.0.last(), one.0.last());
        assert_eq!(many.1.last(), one.1.last());
        assert_eq!(one.2, 100000);
    }
}

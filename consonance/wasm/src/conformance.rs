// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::{
    admission::{AdmittedModule, Profile},
    runtime::{Invocation, Runtime, Scalar},
    services::Host,
};
use environment::input_spec::{InputSpec, nominal_factory};
use serde_json::Value;
use std::{path::Path, sync::Arc};
use wasmi::{Val, core::TrapCode};

fn scalar(value: &Value) -> Scalar {
    let bits = value["value"].as_str().unwrap().parse::<u64>().unwrap();
    match value["type"].as_str().unwrap() {
        "i32" => Scalar::I32(bits as u32),
        "i64" => Scalar::I64(bits),
        "f32" => Scalar::F32(bits as u32),
        "f64" => Scalar::F64(bits),
        other => panic!("unsupported upstream scalar {other}"),
    }
}
fn matches(actual: &Val, expected: &Value) -> bool {
    let text = expected["value"].as_str().unwrap();
    if text.starts_with("nan:") {
        return match actual {
            Val::F32(value) if expected["type"] == "f32" => {
                let bits = value.to_bits() & 0x7fff_ffff;
                if text == "nan:canonical" {
                    bits == 0x7fc0_0000
                } else {
                    bits & 0x7fc0_0000 == 0x7fc0_0000
                }
            }
            Val::F64(value) if expected["type"] == "f64" => {
                let bits = value.to_bits() & 0x7fff_ffff_ffff_ffff;
                if text == "nan:canonical" {
                    bits == 0x7ff8_0000_0000_0000
                } else {
                    bits & 0x7ff8_0000_0000_0000 == 0x7ff8_0000_0000_0000
                }
            }
            _ => false,
        };
    }
    match (actual, scalar(expected)) {
        (Val::I32(a), Scalar::I32(b)) => *a as u32 == b,
        (Val::I64(a), Scalar::I64(b)) => *a as u64 == b,
        (Val::F32(a), Scalar::F32(b)) => a.to_bits() == b,
        (Val::F64(a), Scalar::F64(b)) => a.to_bits() == b,
        _ => false,
    }
}
fn value(scalar: Scalar) -> Val {
    match scalar {
        Scalar::I32(x) => Val::I32(x as i32),
        Scalar::I64(x) => Val::I64(x as i64),
        Scalar::F32(x) => Val::F32(wasmi::core::F32::from_bits(x)),
        Scalar::F64(x) => Val::F64(wasmi::core::F64::from_bits(x)),
    }
}

#[test]
#[ignore = "requires generated pinned upstream scalar fixtures"]
#[cfg(not(miri))]
fn upstream_scalar_conformance() {
    let directory = std::env::var("WASM_CONFORMANCE_DIR").expect("prepared conformance directory");
    let directory = Path::new(&directory);
    let pin: Value =
        serde_json::from_slice(&std::fs::read(directory.join("pin.json")).unwrap()).unwrap();
    assert_eq!(pin["revision"], "957c932e7158c5a6891be68ca424aaa0aa505f97");
    let profile = Profile {
        memory_pages: 1,
        ..Profile::default()
    };
    for suite in pin["suites"].as_array().unwrap() {
        let suite = suite.as_str().unwrap();
        let script: Value = serde_json::from_slice(
            &std::fs::read(directory.join(format!("{suite}.json"))).unwrap(),
        )
        .unwrap();
        let commands = script["commands"].as_array().unwrap();
        let source =
            std::fs::read(directory.join(commands[0]["filename"].as_str().unwrap())).unwrap();
        assert_eq!(commands[0]["type"], "module");
        let first = &commands
            .iter()
            .find(|command| command["type"] == "assert_return")
            .unwrap()["action"];
        let admitted = AdmittedModule::new(&source, profile.clone()).unwrap();
        let host = Host::new(
            InputSpec::seeded(7)
                .materialize(&nominal_factory())
                .unwrap(),
        );
        let mut runtime = Runtime::new(
            Arc::new(admitted),
            host,
            Invocation {
                name: first["field"].as_str().unwrap().into(),
                arguments: first["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(scalar)
                    .collect(),
            },
        )
        .unwrap();
        let mut returns = 0;
        let mut traps = 0;
        let mut rejected = 0;
        for command in commands.iter().skip(1) {
            let kind = command["type"].as_str().unwrap();
            if kind == "assert_invalid" || kind == "assert_malformed" {
                let bytes =
                    std::fs::read(directory.join(command["filename"].as_str().unwrap())).unwrap();
                assert!(AdmittedModule::new(&bytes, profile.clone()).is_err());
                rejected += 1;
                continue;
            }
            assert!(
                kind == "assert_return" || kind == "assert_trap",
                "unexpected directive {command}"
            );
            let action = &command["action"];
            assert_eq!(action["type"], "invoke");
            let function = runtime
                .instance
                .get_func(&runtime.store, action["field"].as_str().unwrap())
                .unwrap();
            let arguments: Vec<_> = action["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|arg| value(scalar(arg)))
                .collect();
            let expected = command["expected"].as_array().unwrap();
            let mut outputs: Vec<_> = function
                .ty(&runtime.store)
                .results()
                .iter()
                .copied()
                .map(Val::default)
                .collect();
            runtime.store.set_fuel(10_000_000).unwrap();
            let result = function.call(&mut runtime.store, &arguments, &mut outputs);
            if kind == "assert_trap" {
                let expected = match command["text"].as_str().unwrap() {
                    "integer divide by zero" => TrapCode::IntegerDivisionByZero,
                    "integer overflow" => TrapCode::IntegerOverflow,
                    "invalid conversion to integer" => TrapCode::BadConversionToInteger,
                    text => panic!("unrecognized upstream trap {text}"),
                };
                assert_eq!(
                    result.unwrap_err().as_trap_code(),
                    Some(expected),
                    "{suite}: {command}"
                );
                traps += 1;
            } else {
                result.unwrap();
                assert_eq!(outputs.len(), expected.len());
                for (actual, expected) in outputs.iter().zip(expected) {
                    assert!(
                        matches(actual, expected),
                        "{suite}: {command}; got {actual:?}"
                    );
                }
                returns += 1;
            }
        }
        println!(
            "WASM_CONFORMANCE {{\"suite\":\"{suite}\",\"returns\":{returns},\"traps\":{traps},\"admission_rejections\":{rejected}}}"
        );
    }
}

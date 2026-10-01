// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperatorMap {
    pub source: [u64; 2],
    pub admitted: [u64; 2],
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FunctionMap {
    pub function: u32,
    pub source: [u64; 2],
    pub admitted: [u64; 2],
    pub operators: Vec<OperatorMap>,
}

impl FunctionMap {
    pub fn source_position(&self, admitted: u64) -> Option<u64> {
        if admitted == 0 {
            return self.operators.first().map(|op| op.source[0]);
        }
        if admitted == self.admitted[1] {
            return Some(self.source[1]);
        }
        if admitted >= self.admitted[0] && admitted < self.operators.first()?.admitted[0] {
            return Some(self.operators.first()?.source[0]);
        }
        self.operators
            .iter()
            .find(|op| op.admitted[0] <= admitted && admitted < op.admitted[1])
            .map(|op| op.source[0])
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompiledLocation {
    pub instruction: u64,
    pub admitted: [u64; 2],
    pub source: Option<[u64; 2]>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompiledFunction {
    pub function: u32,
    pub compiled_function: u32,
    pub locations: Vec<CompiledLocation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DebugMap {
    pub version: u32,
    pub source_digest: [u8; 32],
    pub admitted_digest: [u8; 32],
    pub execution_digest: [u8; 32],
    pub source_code_section_start: u64,
    pub functions: Vec<FunctionMap>,
    pub compiled: Vec<CompiledFunction>,
}

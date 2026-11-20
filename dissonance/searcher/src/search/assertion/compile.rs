// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    catalog::{Catalog, NONE, RECORD_SLOTS, Record},
    expr::{self, Binary, Expr},
    key::{AssertionKey, MAX_PREFERENCES, Wide},
    strategy::{Lines, Shared, StageLines, Steps, Strategy},
};
use crate::search::archive::{KeyShape, MAX_ENTRIES_PER_KEY};

pub const MAX_AGGREGATES: usize = 8;
pub const OBJECTIVE_WIDTH: usize = 4;
const STACK: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Cmp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Cmp {
    fn holds(self, ordering: std::cmp::Ordering) -> bool {
        match self {
            Self::Eq => ordering.is_eq(),
            Self::Ne => ordering.is_ne(),
            Self::Lt => ordering.is_lt(),
            Self::Le => ordering.is_le(),
            Self::Gt => ordering.is_gt(),
            Self::Ge => ordering.is_ge(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Op {
    Const(i64),
    Load(u8),
    Aggregate(u8),
    Local(u8),
    Add,
    Sub,
    Mul,
    Div(i64),
    Rem(i64),
    Neg,
    Not,
    And,
    Or,
    Compare(Cmp),
    Lexicographic(u8, Cmp),
    Min,
    Max,
    Select,
    Popcount,
    In(u16),
    Index(u16),
    Bucket(u16),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Span {
    lo: i64,
    hi: i64,
    none: bool,
}

impl Span {
    const FLAG: Self = Self {
        lo: 0,
        hi: 1,
        none: false,
    };

    fn exact(lo: i128, hi: i128, none: bool) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            lo: i64::try_from(lo).map_err(|_| "an expression range overflows")?,
            hi: i64::try_from(hi).map_err(|_| "an expression range overflows")?,
            none,
        })
    }
}

#[derive(Clone, Debug)]
struct Aggregate {
    value: Vec<Op>,
    since: Option<Vec<Op>>,
}

#[derive(Clone, Copy, Debug)]
struct Lane {
    lo: i64,
    hi: i64,
    floor: i64,
    radix: u64,
    step: i64,
    none: bool,
    less: bool,
}

impl Lane {
    fn new(span: Span, step: i64, less: bool) -> Result<Self, Box<dyn Error>> {
        if step <= 0 {
            return Err("a step must be positive".into());
        }
        let floor = span.lo.div_euclid(step);
        let buckets = i128::from(span.hi.div_euclid(step)) - i128::from(floor) + 1;
        let radix =
            u64::try_from(buckets + i128::from(span.none)).map_err(|_| "a lane range overflows")?;
        Ok(Self {
            lo: span.lo,
            hi: span.hi,
            floor,
            radix,
            step,
            none: span.none,
            less,
        })
    }

    fn pack(self, value: i64) -> u64 {
        let packed = if value == NONE {
            0
        } else {
            let bucket = i128::from(value.clamp(self.lo, self.hi).div_euclid(self.step))
                - i128::from(self.floor);
            u64::try_from(bucket).unwrap_or(u64::MAX) + u64::from(self.none)
        };
        if self.less {
            self.radix - 1 - packed
        } else {
            packed
        }
    }

    fn unpack(self, packed: u64) -> i64 {
        let packed = if self.less {
            self.radix - 1 - packed
        } else {
            packed
        };
        if self.none && packed == 0 {
            return NONE;
        }
        let value = (i128::from(packed - u64::from(self.none)) + i128::from(self.floor))
            * i128::from(self.step);
        i64::try_from(value).unwrap_or(if value < 0 { i64::MIN } else { i64::MAX })
    }
}

#[derive(Clone, Debug)]
struct Tuple {
    ops: Vec<Op>,
    lanes: Vec<Lane>,
}

impl Tuple {
    fn empty() -> Self {
        Self {
            ops: Vec::new(),
            lanes: Vec::new(),
        }
    }

    fn radix(&self) -> u128 {
        self.lanes.iter().fold(1_u128, |total, lane| {
            total.saturating_mul(u128::from(lane.radix))
        })
    }

    fn fits(&self, limit: u128, line: &str) -> Result<(), Box<dyn Error>> {
        if self.radix() > limit {
            return Err(format!(
                "`{line}` has {} values and its key field holds {limit}",
                self.radix()
            )
            .into());
        }
        Ok(())
    }

    fn pack(&self, env: &Env<'_>, stack: &mut [i64; STACK]) -> u64 {
        if self.lanes.is_empty() {
            return 0;
        }
        let depth = eval(&self.ops, env, stack);
        let start = depth - self.lanes.len();
        self.lanes
            .iter()
            .zip(&stack[start..depth])
            .fold(0_u64, |packed, (lane, value)| {
                packed
                    .wrapping_mul(lane.radix)
                    .wrapping_add(lane.pack(*value))
            })
    }

    fn unpack(&self, mut packed: u64) -> Vec<i64> {
        let mut values = vec![0; self.lanes.len()];
        for (lane, value) in self.lanes.iter().zip(values.iter_mut()).rev() {
            *value = lane.unpack(packed % lane.radix);
            packed /= lane.radix;
        }
        values
    }
}

#[derive(Clone, Debug)]
struct LineSet {
    absent: Option<Vec<Op>>,
    reject: Vec<(String, Vec<Op>)>,
    spread: Tuple,
    identity: Tuple,
    prefer: Vec<Tuple>,
    scope: Option<Tuple>,
    across: Tuple,
}

#[derive(Clone, Debug)]
struct Stage {
    name: String,
    goal: Vec<Op>,
    objective: Tuple,
    latch: bool,
    lines: Option<LineSet>,
    reachable: bool,
}

#[derive(Clone, Debug)]
enum Progress {
    Flat,
    Tier(Tuple),
    Stages { stages: Vec<Stage>, radix: u64 },
}

#[derive(Clone, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Accumulator {
    started: bool,
    stage: u8,
    values: Box<[i64]>,
}

impl Accumulator {
    #[must_use]
    pub fn stage(&self) -> Option<u8> {
        self.started.then_some(self.stage)
    }

    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        std::mem::size_of_val::<[i64]>(&self.values)
    }
}

#[derive(Clone, Copy, Default)]
struct Held {
    started: bool,
    stage: u8,
    objective: [i64; OBJECTIVE_WIDTH],
    values: [i64; MAX_AGGREGATES],
    anchors: [i64; MAX_AGGREGATES],
}

impl Held {
    fn stage(&self) -> Option<u8> {
        self.started.then_some(self.stage)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Observed {
    pub key: Option<AssertionKey>,
    pub rejected: Option<u8>,
    pub goal: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct KeyView {
    pub stage: Option<String>,
    pub progress: Vec<i64>,
    pub place: Vec<i64>,
    pub identity: Vec<i64>,
}

struct Env<'a> {
    record: &'a [i64; RECORD_SLOTS],
    aggregates: &'a [i64; MAX_AGGREGATES],
    locals: &'a [i64; OBJECTIVE_WIDTH],
    tables: &'a [Vec<i64>],
}

fn truthy(value: i64) -> bool {
    value != 0 && value != NONE
}

fn eval(ops: &[Op], env: &Env<'_>, stack: &mut [i64; STACK]) -> usize {
    let mut depth = 0;
    macro_rules! pop {
        () => {{
            depth -= 1;
            stack[depth]
        }};
    }
    macro_rules! push {
        ($value:expr) => {{
            let value = $value;
            stack[depth] = value;
            depth += 1;
        }};
    }
    macro_rules! arithmetic {
        ($op:expr) => {{
            let right = pop!();
            let left = pop!();
            push!(if left == NONE || right == NONE {
                NONE
            } else {
                $op(left, right)
            });
        }};
    }
    for op in ops {
        match *op {
            Op::Const(value) => push!(value),
            Op::Load(slot) => push!(env.record[usize::from(slot)]),
            Op::Aggregate(slot) => push!(env.aggregates[usize::from(slot)]),
            Op::Local(slot) => push!(env.locals[usize::from(slot)]),
            Op::Add => arithmetic!(i64::saturating_add),
            Op::Sub => arithmetic!(i64::saturating_sub),
            Op::Mul => arithmetic!(i64::saturating_mul),
            Op::Div(divisor) => {
                let value = pop!();
                push!(if value == NONE {
                    NONE
                } else {
                    value.div_euclid(divisor)
                });
            }
            Op::Rem(divisor) => {
                let value = pop!();
                push!(if value == NONE {
                    NONE
                } else {
                    value.rem_euclid(divisor)
                });
            }
            Op::Neg => {
                let value = pop!();
                push!(if value == NONE {
                    NONE
                } else {
                    value.saturating_neg()
                });
            }
            Op::Not => {
                let value = pop!();
                push!(i64::from(!truthy(value)));
            }
            Op::And => {
                let right = pop!();
                let left = pop!();
                push!(i64::from(truthy(left) && truthy(right)));
            }
            Op::Or => {
                let right = pop!();
                let left = pop!();
                push!(i64::from(truthy(left) || truthy(right)));
            }
            Op::Compare(cmp) => {
                let right = pop!();
                let left = pop!();
                push!(i64::from(if left == NONE || right == NONE {
                    matches!(cmp, Cmp::Eq) && left == right
                        || matches!(cmp, Cmp::Ne) && left != right
                } else {
                    cmp.holds(left.cmp(&right))
                }));
            }
            Op::Lexicographic(width, cmp) => {
                let width = usize::from(width);
                let left = &stack[depth - 2 * width..depth - width];
                let right = &stack[depth - width..depth];
                let ordering = left.cmp(right);
                depth -= 2 * width;
                push!(i64::from(cmp.holds(ordering)));
            }
            Op::Min | Op::Max => {
                let right = pop!();
                let left = pop!();
                push!(match (left == NONE, right == NONE) {
                    (true, _) => right,
                    (_, true) => left,
                    _ if matches!(op, Op::Min) => left.min(right),
                    _ => left.max(right),
                });
            }
            Op::Select => {
                let otherwise = pop!();
                let then = pop!();
                let condition = pop!();
                push!(if truthy(condition) { then } else { otherwise });
            }
            Op::Popcount => {
                let value = pop!();
                push!(if value == NONE {
                    NONE
                } else {
                    i64::from(value.count_ones())
                });
            }
            Op::In(table) => {
                let value = pop!();
                push!(i64::from(env.tables[usize::from(table)].contains(&value)));
            }
            Op::Index(table) => {
                let value = pop!();
                push!(
                    env.tables[usize::from(table)]
                        .iter()
                        .position(|entry| *entry == value)
                        .map_or(NONE, |index| i64::try_from(index).unwrap_or(NONE))
                );
            }
            Op::Bucket(table) => {
                let value = pop!();
                push!(if value == NONE {
                    NONE
                } else {
                    i64::try_from(
                        env.tables[usize::from(table)]
                            .iter()
                            .filter(|bound| **bound <= value)
                            .count(),
                    )
                    .unwrap_or(NONE)
                });
            }
        }
    }
    depth
}

fn depth_of(ops: &[Op]) -> Result<usize, Box<dyn Error>> {
    let mut depth: isize = 0;
    let mut deepest = 0;
    for op in ops {
        depth += match op {
            Op::Const(_) | Op::Load(_) | Op::Aggregate(_) | Op::Local(_) => 1,
            Op::Div(_)
            | Op::Rem(_)
            | Op::Neg
            | Op::Not
            | Op::Popcount
            | Op::In(_)
            | Op::Index(_)
            | Op::Bucket(_) => 0,
            Op::Select => -2,
            Op::Lexicographic(width, _) => 1 - 2 * isize::from(*width),
            _ => -1,
        };
        deepest = deepest.max(depth);
    }
    let deepest = usize::try_from(deepest)?;
    if deepest > STACK {
        return Err(format!("an expression needs {deepest} stack values and {STACK} exist").into());
    }
    Ok(deepest)
}

struct Compiler<'a> {
    catalog: &'a Catalog,
    tables: &'a BTreeMap<String, Vec<i64>>,
    defines: &'a BTreeMap<String, String>,
    resolving: Vec<String>,
    table_order: Vec<String>,
    aggregates: Vec<Aggregate>,
    aggregate_spans: Vec<Span>,
    in_aggregate: bool,
    locals: Option<Vec<Span>>,
}

impl Compiler<'_> {
    fn defined(&mut self, name: &str) -> Result<Option<Expr>, Box<dyn Error>> {
        let Some(text) = self.defines.get(name) else {
            return Ok(None);
        };
        if self.resolving.iter().any(|resolving| resolving == name) {
            return Err(format!("`{name}` is defined in terms of itself").into());
        }
        Ok(Some(expr::parse(text)?))
    }

    fn expand<T>(
        &mut self,
        name: &str,
        expr: &Expr,
        compile: impl FnOnce(&mut Self, &Expr) -> Result<T, Box<dyn Error>>,
    ) -> Result<T, Box<dyn Error>> {
        self.resolving.push(name.to_owned());
        let result = compile(self, expr);
        self.resolving.pop();
        result
    }

    fn table(&self, expr: &Expr) -> Result<(u16, &Vec<i64>), Box<dyn Error>> {
        let Expr::Name(name) = expr else {
            return Err("a table argument must name a table".into());
        };
        let index = self
            .table_order
            .iter()
            .position(|table| table == name)
            .ok_or_else(|| format!("no table is named `{name}`"))?;
        Ok((u16::try_from(index)?, &self.tables[name]))
    }

    fn name(&mut self, name: &str, ops: &mut Vec<Op>) -> Result<Span, Box<dyn Error>> {
        if let Some(expr) = self.defined(name)? {
            return self.expand(name, &expr, |compiler, expr| compiler.scalar(expr, ops));
        }
        if name == "objective"
            && let Some(locals) = &self.locals
        {
            ops.push(Op::Local(0));
            return Ok(locals[0]);
        }
        if let Some(slot) = self.catalog.slot(name) {
            let signal = &self.catalog.signals()[slot];
            ops.push(Op::Load(u8::try_from(slot)?));
            return Ok(Span {
                lo: signal.min,
                hi: signal.max,
                none: false,
            });
        }
        if self.catalog.group(name).is_some() {
            return Err(format!("`{name}` is a tuple and is used as a number").into());
        }
        Err(format!("`{name}` is not in the catalog").into())
    }

    fn items(&mut self, expr: &Expr, ops: &mut Vec<Op>) -> Result<Vec<Span>, Box<dyn Error>> {
        if let Expr::Name(name) = expr
            && let Some(defined) = self.defined(name)?
        {
            return self.expand(name, &defined, |compiler, expr| compiler.items(expr, ops));
        }
        match expr {
            Expr::Tuple(items) => {
                let mut spans = Vec::new();
                for item in items {
                    spans.extend(self.items(item, ops)?);
                }
                Ok(spans)
            }
            Expr::Name(name) if self.catalog.group(name).is_some() => {
                let slots = self.catalog.group(name).unwrap_or_default();
                slots
                    .map(|slot| {
                        let signal = &self.catalog.signals()[slot];
                        ops.push(Op::Load(u8::try_from(slot)?));
                        Ok(Span {
                            lo: signal.min,
                            hi: signal.max,
                            none: false,
                        })
                    })
                    .collect()
            }
            expr => Ok(vec![self.scalar(expr, ops)?]),
        }
    }

    fn scalar(&mut self, expr: &Expr, ops: &mut Vec<Op>) -> Result<Span, Box<dyn Error>> {
        match expr {
            Expr::Int(value) => {
                ops.push(Op::Const(*value));
                Ok(Span {
                    lo: *value,
                    hi: *value,
                    none: false,
                })
            }
            Expr::None => {
                ops.push(Op::Const(NONE));
                Ok(Span {
                    lo: 0,
                    hi: 0,
                    none: true,
                })
            }
            Expr::Name(name) => self.name(name, ops),
            Expr::Tuple(_) => Err("a tuple is used as a number".into()),
            Expr::Neg(inner) => {
                let span = self.scalar(inner, ops)?;
                ops.push(Op::Neg);
                Span::exact(-i128::from(span.hi), -i128::from(span.lo), span.none)
            }
            Expr::Not(inner) => {
                self.scalar(inner, ops)?;
                ops.push(Op::Not);
                Ok(Span::FLAG)
            }
            Expr::If(condition, then, otherwise) => {
                self.scalar(condition, ops)?;
                let then = self.scalar(then, ops)?;
                let otherwise = self.scalar(otherwise, ops)?;
                ops.push(Op::Select);
                Ok(
                    match (
                        then.none && then.lo == 0 && then.hi == 0,
                        otherwise.none && otherwise.lo == 0 && otherwise.hi == 0,
                    ) {
                        (true, _) => Span {
                            none: true,
                            ..otherwise
                        },
                        (_, true) => Span { none: true, ..then },
                        _ => Span {
                            lo: then.lo.min(otherwise.lo),
                            hi: then.hi.max(otherwise.hi),
                            none: then.none || otherwise.none,
                        },
                    },
                )
            }
            Expr::Binary(op, left, right) => self.binary(*op, left, right, ops),
            Expr::Call(function, args) => self.call(function, args, ops),
        }
    }

    fn binary(
        &mut self,
        op: Binary,
        left: &Expr,
        right: &Expr,
        ops: &mut Vec<Op>,
    ) -> Result<Span, Box<dyn Error>> {
        let cmp = match op {
            Binary::Eq => Some(Cmp::Eq),
            Binary::Ne => Some(Cmp::Ne),
            Binary::Lt => Some(Cmp::Lt),
            Binary::Le => Some(Cmp::Le),
            Binary::Gt => Some(Cmp::Gt),
            Binary::Ge => Some(Cmp::Ge),
            _ => None,
        };
        if let Some(cmp) = cmp {
            let tuple = |expr: &Expr| {
                matches!(expr, Expr::Tuple(_))
                    || matches!(expr, Expr::Name(name) if self.catalog.group(name).is_some()
                    || self.defines.get(name).is_some_and(|text| {
                        matches!(expr::parse(text), Ok(Expr::Tuple(_)))
                    }))
            };
            if tuple(left) || tuple(right) {
                let width = self.items(left, ops)?.len();
                if self.items(right, ops)?.len() != width {
                    return Err("compared tuples differ in width".into());
                }
                ops.push(Op::Lexicographic(u8::try_from(width)?, cmp));
            } else {
                self.scalar(left, ops)?;
                self.scalar(right, ops)?;
                ops.push(Op::Compare(cmp));
            }
            return Ok(Span::FLAG);
        }
        if matches!(op, Binary::Div | Binary::Rem) {
            let Expr::Int(divisor) = right else {
                return Err("a divisor must be a positive literal".into());
            };
            if *divisor <= 0 {
                return Err("a divisor must be a positive literal".into());
            }
            let span = self.scalar(left, ops)?;
            return if op == Binary::Div {
                ops.push(Op::Div(*divisor));
                Ok(Span {
                    lo: span.lo.div_euclid(*divisor),
                    hi: span.hi.div_euclid(*divisor),
                    none: span.none,
                })
            } else {
                ops.push(Op::Rem(*divisor));
                Ok(if span.lo >= 0 && span.hi < *divisor {
                    span
                } else {
                    Span {
                        lo: 0,
                        hi: divisor - 1,
                        none: span.none,
                    }
                })
            };
        }
        let a = self.scalar(left, ops)?;
        let b = self.scalar(right, ops)?;
        let none = a.none || b.none;
        let (alo, ahi, blo, bhi) = (
            i128::from(a.lo),
            i128::from(a.hi),
            i128::from(b.lo),
            i128::from(b.hi),
        );
        match op {
            Binary::Add => {
                ops.push(Op::Add);
                Span::exact(alo + blo, ahi + bhi, none)
            }
            Binary::Sub => {
                ops.push(Op::Sub);
                Span::exact(alo - bhi, ahi - blo, none)
            }
            Binary::Mul => {
                ops.push(Op::Mul);
                let products = [alo * blo, alo * bhi, ahi * blo, ahi * bhi];
                Span::exact(
                    products.into_iter().min().unwrap_or(0),
                    products.into_iter().max().unwrap_or(0),
                    none,
                )
            }
            Binary::And => {
                ops.push(Op::And);
                Ok(Span::FLAG)
            }
            Binary::Or => {
                ops.push(Op::Or);
                Ok(Span::FLAG)
            }
            _ => Err("unsupported operator".into()),
        }
    }

    fn call(
        &mut self,
        function: &str,
        args: &[Expr],
        ops: &mut Vec<Op>,
    ) -> Result<Span, Box<dyn Error>> {
        let arity = |count: std::ops::RangeInclusive<usize>| {
            if count.contains(&args.len()) {
                Ok(())
            } else {
                Err(format!("`{function}` takes {count:?} arguments"))
            }
        };
        match function {
            "min" | "max" => {
                if args.len() < 2 {
                    return Err(format!("`{function}` takes at least two arguments").into());
                }
                let mut span = self.scalar(&args[0], ops)?;
                for arg in &args[1..] {
                    let next = self.scalar(arg, ops)?;
                    let min = function == "min";
                    ops.push(if min { Op::Min } else { Op::Max });
                    let (mut lo, mut hi) = if min {
                        (span.lo.min(next.lo), span.hi.min(next.hi))
                    } else {
                        (span.lo.max(next.lo), span.hi.max(next.hi))
                    };
                    if span.none {
                        (lo, hi) = (lo.min(next.lo), hi.max(next.hi));
                    }
                    if next.none {
                        (lo, hi) = (lo.min(span.lo), hi.max(span.hi));
                    }
                    span = Span {
                        lo,
                        hi,
                        none: span.none && next.none,
                    };
                }
                Ok(span)
            }
            "step" => {
                arity(2..=2)?;
                self.binary(Binary::Div, &args[0], &args[1], ops)
            }
            "popcount" => {
                arity(1..=1)?;
                let span = self.scalar(&args[0], ops)?;
                if span.lo < 0 {
                    return Err("popcount needs a value that is never negative".into());
                }
                ops.push(Op::Popcount);
                Ok(Span {
                    lo: 0,
                    hi: i64::from(64 - span.hi.leading_zeros()),
                    none: span.none,
                })
            }
            "in" | "index" | "bucket" => {
                arity(2..=2)?;
                let (table, entries) = self.table(&args[1])?;
                let len = i64::try_from(entries.len())?;
                self.scalar(&args[0], ops)?;
                Ok(match function {
                    "in" => {
                        ops.push(Op::In(table));
                        Span::FLAG
                    }
                    "index" => {
                        ops.push(Op::Index(table));
                        Span {
                            lo: 0,
                            hi: (len - 1).max(0),
                            none: true,
                        }
                    }
                    _ => {
                        ops.push(Op::Bucket(table));
                        Span {
                            lo: 0,
                            hi: len,
                            none: false,
                        }
                    }
                })
            }
            AGGREGATE => {
                arity(1..=2)?;
                if self.in_aggregate {
                    return Err("an aggregate cannot contain another aggregate".into());
                }
                if self.aggregates.len() == MAX_AGGREGATES {
                    return Err(
                        format!("a strategy holds at most {MAX_AGGREGATES} aggregates").into(),
                    );
                }
                self.in_aggregate = true;
                let mut value = Vec::new();
                let span = self.scalar(&args[0], &mut value);
                let since = args
                    .get(1)
                    .map(|since| {
                        let mut ops = Vec::new();
                        self.scalar(since, &mut ops).map(|_| ops)
                    })
                    .transpose();
                self.in_aggregate = false;
                let (span, since) = (span?, since?);
                depth_of(&value)?;
                if let Some(since) = &since {
                    depth_of(since)?;
                }
                ops.push(Op::Aggregate(u8::try_from(self.aggregates.len())?));
                self.aggregates.push(Aggregate { value, since });
                self.aggregate_spans.push(span);
                Ok(span)
            }
            _ => Err(format!("`{function}` is not a strategy function").into()),
        }
    }

    fn line(&mut self, text: &str, ops: &mut Vec<Op>) -> Result<Vec<Span>, Box<dyn Error>> {
        let text = text.trim();
        if self.catalog.slot(text).is_some() || self.catalog.group(text).is_some() {
            return self.items(&Expr::Name(text.to_owned()), ops);
        }
        let expr = expr::parse(text)?;
        self.items(&expr, ops)
    }

    fn condition(&mut self, text: &str) -> Result<Vec<Op>, Box<dyn Error>> {
        let mut ops = Vec::new();
        if self.line(text, &mut ops)?.len() != 1 {
            return Err(format!("`{text}` is a tuple and is used as a condition").into());
        }
        depth_of(&ops)?;
        Ok(ops)
    }

    fn goal_text(&self, text: &str) -> String {
        match self.catalog.declaration(text.trim()) {
            Some(declaration) if declaration.form == "sometimes_all" => {
                format!("'{}' == {}", declaration.name, declaration.slots.len() - 1)
            }
            _ => text.to_owned(),
        }
    }

    fn goal(&mut self, text: &str) -> Result<Vec<Op>, Box<dyn Error>> {
        let text = self.goal_text(text);
        self.condition(&text)
    }

    fn reachable(&self, text: &str) -> bool {
        let text = text.trim();
        let expr = if self.catalog.slot(text).is_some() {
            Expr::Name(text.to_owned())
        } else {
            match expr::parse(text) {
                Ok(expr) => expr,
                Err(_) => return true,
            }
        };
        let mut scratch = Compiler {
            catalog: self.catalog,
            tables: self.tables,
            defines: self.defines,
            resolving: Vec::new(),
            table_order: self.table_order.clone(),
            aggregates: Vec::new(),
            aggregate_spans: Vec::new(),
            in_aggregate: false,
            locals: self.locals.clone(),
        };
        scratch.truth(&expr) != Some(false)
    }

    fn span(&mut self, expr: &Expr) -> Option<Span> {
        self.scalar(expr, &mut Vec::new()).ok()
    }

    fn span_truth(&mut self, expr: &Expr) -> Option<bool> {
        let span = self.span(expr)?;
        if span.lo == 0 && span.hi == 0 {
            Some(false)
        } else if span.none || (span.lo <= 0 && span.hi >= 0) {
            None
        } else {
            Some(true)
        }
    }

    fn truth(&mut self, expr: &Expr) -> Option<bool> {
        match expr {
            Expr::Name(name) => match self.defined(name) {
                Ok(Some(defined)) => self
                    .expand(name, &defined, |compiler, expr| Ok(compiler.truth(expr)))
                    .ok()
                    .flatten(),
                _ => self.span_truth(expr),
            },
            Expr::Not(inner) => self.truth(inner).map(|holds| !holds),
            Expr::Binary(Binary::And, left, right) => match (self.truth(left), self.truth(right)) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            },
            Expr::Binary(Binary::Or, left, right) => match (self.truth(left), self.truth(right)) {
                (Some(true), _) | (_, Some(true)) => Some(true),
                (Some(false), Some(false)) => Some(false),
                _ => None,
            },
            Expr::Binary(
                op @ (Binary::Eq | Binary::Ne | Binary::Lt | Binary::Le | Binary::Gt | Binary::Ge),
                left,
                right,
            ) => {
                let (a, b) = (self.span(left)?, self.span(right)?);
                if a.none || b.none {
                    return None;
                }
                let overlap = a.lo <= b.hi && b.lo <= a.hi;
                let single = a.lo == a.hi && b.lo == b.hi && a.lo == b.lo;
                let (possible, always) = match op {
                    Binary::Eq => (overlap, single),
                    Binary::Ne => (!single, !overlap),
                    Binary::Lt => (a.lo < b.hi, a.hi < b.lo),
                    Binary::Le => (a.lo <= b.hi, a.hi <= b.lo),
                    Binary::Gt => (a.hi > b.lo, a.lo > b.hi),
                    _ => (a.hi >= b.lo, a.lo >= b.hi),
                };
                if always {
                    Some(true)
                } else if possible {
                    None
                } else {
                    Some(false)
                }
            }
            _ => self.span_truth(expr),
        }
    }

    fn tuple(&mut self, items: &[&str], limit: u128, line: &str) -> Result<Tuple, Box<dyn Error>> {
        let mut ops = Vec::new();
        let mut lanes = Vec::new();
        for item in items {
            for span in self.line(item, &mut ops)? {
                lanes.push(Lane::new(span, 1, false)?);
            }
        }
        depth_of(&ops)?;
        let tuple = Tuple { ops, lanes };
        tuple.fits(limit, line)?;
        Ok(tuple)
    }

    fn preference(
        &mut self,
        items: &[String],
        limit: u128,
        line: &str,
    ) -> Result<Tuple, Box<dyn Error>> {
        let mut ops = Vec::new();
        let mut lanes = Vec::new();
        for item in items {
            let (text, direction) = item
                .rsplit_once(':')
                .ok_or_else(|| format!("`{item}` needs `: more` or `: less`"))?;
            let less = match direction.trim() {
                "more" => false,
                "less" => true,
                other => return Err(format!("`{other}` is not `more` or `less`").into()),
            };
            for span in self.line(text, &mut ops)? {
                lanes.push(Lane::new(span, 1, less)?);
            }
        }
        depth_of(&ops)?;
        let tuple = Tuple { ops, lanes };
        tuple.fits(limit, line)?;
        Ok(tuple)
    }

    fn lines(
        &mut self,
        lines: &Shared,
        fallback: Option<&LineSet>,
    ) -> Result<LineSet, Box<dyn Error>> {
        fn items(lines: &Option<Lines>) -> Option<Vec<&str>> {
            lines.as_ref().map(Lines::items)
        }
        let absent = match (&lines.absent, fallback) {
            (Some(absent), _) => Some(self.condition(absent)?),
            (None, Some(fallback)) => fallback.absent.clone(),
            (None, None) => None,
        };
        let reject = match (items(&lines.reject), fallback) {
            (Some(reject), _) => reject
                .into_iter()
                .map(|line| Ok((line.to_owned(), self.condition(line)?)))
                .collect::<Result<_, Box<dyn Error>>>()?,
            (None, Some(fallback)) => fallback.reject.clone(),
            (None, None) => Vec::new(),
        };
        let spread = match (items(&lines.spread), fallback) {
            (Some(spread), _) => self.tuple(&spread, u128::from(u64::MAX) + 1, "spread")?,
            (None, Some(fallback)) => fallback.spread.clone(),
            (None, None) => Tuple::empty(),
        };
        let identity = match (items(&lines.identity), fallback) {
            (Some(identity), _) => self.tuple(&identity, u128::from(u64::MAX) + 1, "identity")?,
            (None, Some(fallback)) => fallback.identity.clone(),
            (None, None) => Tuple::empty(),
        };
        let prefer = match (&lines.prefer, fallback) {
            (Some(prefer), _) => {
                if prefer.len() > MAX_PREFERENCES {
                    return Err(format!(
                        "a strategy holds at most {MAX_PREFERENCES} preference lists"
                    )
                    .into());
                }
                prefer
                    .iter()
                    .map(|list| self.preference(list, u128::from(u32::MAX) + 1, "prefer"))
                    .collect::<Result<_, _>>()?
            }
            (None, Some(fallback)) => fallback.prefer.clone(),
            (None, None) => Vec::new(),
        };
        let scope = match (&lines.prefer_scope, fallback) {
            (Some(scope), _) => {
                Some(self.tuple(&[scope.as_str()], u128::from(u16::MAX) + 1, "prefer_scope")?)
            }
            (None, Some(fallback)) => fallback.scope.clone(),
            (None, None) => None,
        };
        let across = match (&lines.prefer_across, fallback) {
            (Some(across), _) => {
                self.preference(across, u128::from(u16::MAX) + 1, "prefer_across")?
            }
            (None, Some(fallback)) => fallback.across.clone(),
            (None, None) => Tuple::empty(),
        };
        Ok(LineSet {
            absent,
            reject,
            spread,
            identity,
            prefer,
            scope,
            across,
        })
    }

    fn stage(&mut self, lines: &StageLines, shared: &LineSet) -> Result<Stage, Box<dyn Error>> {
        let goal_text = lines.goal.trim();
        let declared = self.catalog.declaration(goal_text).filter(|declaration| {
            declaration.form.starts_with("sometimes") || declaration.form == "reachable"
        });
        let (goal, default_objective, default_step) = match declared {
            Some(declaration)
                if declaration.form.starts_with("sometimes_greater")
                    || declaration.form.starts_with("sometimes_less") =>
            {
                let gap = format!("'{}.gap'", declaration.name);
                let objective = if declaration.form.starts_with("sometimes_less") {
                    format!("-{gap}")
                } else {
                    gap
                };
                let unit = self.catalog.signals()[declaration.slots.start + 1]
                    .unit
                    .max(1);
                (format!("'{}'", declaration.name), objective, unit)
            }
            Some(declaration) if declaration.form == "sometimes_all" => (
                format!("'{}' == {}", declaration.name, declaration.slots.len() - 1),
                format!("'{}'", declaration.name),
                1,
            ),
            Some(declaration) => (format!("'{}'", declaration.name), "0".to_owned(), 1),
            None => (goal_text.to_owned(), "0".to_owned(), 1),
        };
        let objective_items = lines
            .objective
            .as_ref()
            .map_or_else(|| vec![default_objective.as_str()], Lines::items);
        let mut objective_ops = Vec::new();
        let mut spans = Vec::new();
        for item in &objective_items {
            spans.extend(self.line(item, &mut objective_ops)?);
        }
        if spans.is_empty() || spans.len() > OBJECTIVE_WIDTH {
            return Err(format!("an objective holds from 1 to {OBJECTIVE_WIDTH} values").into());
        }
        let steps = match &lines.step {
            None => {
                let unit = if lines.objective.is_none() {
                    default_step
                } else {
                    objective_items
                        .first()
                        .and_then(|item| self.catalog.slot(item.trim()))
                        .map_or(1, |slot| self.catalog.signals()[slot].unit.max(1))
                };
                let mut steps = vec![1; spans.len()];
                steps[0] = unit;
                steps
            }
            Some(Steps::One(step)) => {
                let mut steps = vec![1; spans.len()];
                steps[0] = *step;
                steps
            }
            Some(Steps::Many(steps)) if steps.len() == spans.len() => steps.clone(),
            Some(Steps::Many(_)) => return Err("a stage gives one step per objective value".into()),
        };
        let lanes = spans
            .iter()
            .zip(&steps)
            .map(|(span, step)| Lane::new(*span, *step, false))
            .collect::<Result<Vec<_>, _>>()?;
        depth_of(&objective_ops)?;
        self.locals = Some(spans);
        let reachable = self.reachable(&goal);
        let goal = self.condition(&goal);
        self.locals = None;
        let stage_lines = lines.lines();
        let overrides = stage_lines != Shared::default();
        let stage_set = overrides
            .then(|| self.lines(&stage_lines, Some(shared)))
            .transpose()?;
        if let Some(set) = &stage_set
            && set.prefer.len() != shared.prefer.len()
        {
            return Err("a stage keeps the shared number of preference lists".into());
        }
        Ok(Stage {
            name: lines.name.clone().unwrap_or_else(|| goal_text.to_owned()),
            goal: goal?,
            objective: Tuple {
                ops: objective_ops,
                lanes,
            },
            latch: lines.latch,
            reachable,
            lines: stage_set,
        })
    }
}

const AGGREGATE: &str = "highest";

fn whole_values(
    items: &[&str],
    catalog: &Catalog,
    defines: &BTreeMap<String, String>,
    into: &mut BTreeSet<String>,
) {
    for item in items {
        let item = item.trim();
        into.insert(item.to_owned());
        let names = if catalog.slot(item).is_some() || catalog.group(item).is_some() {
            vec![item.to_owned()]
        } else {
            match expr::parse(item) {
                Ok(Expr::Name(name)) => vec![name],
                Ok(Expr::Tuple(parts)) => parts
                    .into_iter()
                    .filter_map(|part| match part {
                        Expr::Name(name) => Some(name),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            }
        };
        for name in names {
            if !defines.contains_key(&name)
                && let Some(slots) = catalog.group(&name)
            {
                into.extend(
                    slots
                        .map(|slot| catalog.signals()[slot].name.clone())
                        .filter(|field| !defines.contains_key(field)),
                );
            }
            into.insert(name);
        }
    }
}

fn outside_cell(expr: &Expr, cell: &BTreeSet<String>) -> Option<String> {
    match expr {
        Expr::Int(_) | Expr::None => None,
        Expr::Name(name) => (!cell.contains(name)).then(|| name.clone()),
        Expr::Tuple(items) => items.iter().find_map(|item| outside_cell(item, cell)),
        Expr::Neg(inner) | Expr::Not(inner) => outside_cell(inner, cell),
        Expr::Binary(_, left, right) => {
            outside_cell(left, cell).or_else(|| outside_cell(right, cell))
        }
        Expr::If(condition, then, otherwise) => outside_cell(condition, cell)
            .or_else(|| outside_cell(then, cell))
            .or_else(|| outside_cell(otherwise, cell)),
        Expr::Call(function, _) if function == AGGREGATE => Some(format!("{function}(...)")),
        Expr::Call(function, args) => args.iter().enumerate().find_map(|(index, arg)| match arg {
            Expr::Name(_)
                if index == 1 && ["in", "index", "bucket"].contains(&function.as_str()) =>
            {
                None
            }
            arg => outside_cell(arg, cell),
        }),
    }
}

fn check_scope(
    strategy: &Strategy,
    lines: &LineSet,
    progress: &Progress,
    catalog: &Catalog,
) -> Result<(), Box<dyn Error>> {
    if lines.scope.is_none()
        && !lines.across.lanes.is_empty()
        && !matches!(progress, Progress::Stages { .. })
    {
        return Err(
            "`prefer_across` needs more than one scope: give `prefer_scope` or stages".into(),
        );
    }
    let shared = strategy.lines()?;
    let mut tier = BTreeSet::new();
    if let Some(items) = &strategy.tier {
        whole_values(&items.items(), catalog, &strategy.define, &mut tier);
    }
    let sets = std::iter::once((shared.prefer_scope.as_deref(), shared.spread.as_ref())).chain(
        strategy.stage.iter().map(|stage| {
            (
                stage
                    .prefer_scope
                    .as_deref()
                    .or(shared.prefer_scope.as_deref()),
                stage.spread.as_ref().or(shared.spread.as_ref()),
            )
        }),
    );
    for (scope, spread) in sets {
        let Some(scope) = scope.map(str::trim) else {
            continue;
        };
        let mut cell = tier.clone();
        if let Some(spread) = spread {
            whole_values(&spread.items(), catalog, &strategy.define, &mut cell);
        }
        let expr = if catalog.slot(scope).is_some() || catalog.group(scope).is_some() {
            Expr::Name(scope.to_owned())
        } else {
            expr::parse(scope)?
        };
        if let Some(name) = outside_cell(&expr, &cell) {
            return Err(format!(
                "`prefer_scope` reads `{name}`, which is not a whole value of the progress or `spread`, so the scope is not a function of the cell"
            )
            .into());
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct Compiled {
    policy: String,
    shape: KeyShape,
    signals: usize,
    tables: Vec<Vec<i64>>,
    aggregates: Vec<Aggregate>,
    lines: LineSet,
    progress: Progress,
    goal: Option<Vec<Op>>,
    goal_reachable: bool,
    holds_objective: bool,
}

impl Compiled {
    pub fn new(strategy: &Strategy, catalog: &Catalog) -> Result<Self, Box<dyn Error>> {
        let table_order: Vec<String> = strategy.table.keys().cloned().collect();
        let mut compiler = Compiler {
            catalog,
            tables: &strategy.table,
            defines: &strategy.define,
            resolving: Vec::new(),
            table_order,
            aggregates: Vec::new(),
            aggregate_spans: Vec::new(),
            in_aggregate: false,
            locals: None,
        };
        let lines = compiler.lines(&strategy.lines()?, None)?;
        let goal = strategy
            .goal
            .as_deref()
            .map(|text| compiler.goal(text))
            .transpose()?;
        let goal_reachable = strategy
            .goal
            .as_deref()
            .is_none_or(|text| compiler.reachable(&compiler.goal_text(text)));
        let progress = if let Some(tier) = &strategy.tier {
            Progress::Tier(compiler.tuple(&tier.items(), u128::from(u32::MAX) + 1, "tier")?)
        } else if strategy.stage.is_empty() {
            Progress::Flat
        } else {
            let stages = strategy
                .stage
                .iter()
                .map(|stage| compiler.stage(stage, &lines))
                .collect::<Result<Vec<_>, _>>()?;
            if stages.len() > usize::from(u8::MAX) {
                return Err("a strategy holds at most 255 stages".into());
            }
            let radix = stages
                .iter()
                .map(|stage| stage.objective.radix())
                .max()
                .unwrap_or(1);
            let total = radix.saturating_mul(stages.len() as u128);
            if total > u128::from(u32::MAX) + 1 {
                return Err("the stages and their objectives overflow the progress field".into());
            }
            Progress::Stages {
                stages,
                radix: u64::try_from(radix)?,
            }
        };
        check_scope(strategy, &lines, &progress, catalog)?;
        let holds_objective = compiler.aggregates.iter().any(|aggregate| {
            aggregate
                .value
                .iter()
                .chain(aggregate.since.iter().flatten())
                .any(|op| matches!(op, Op::Local(_)))
        });
        let tables = strategy.table.values().cloned().collect();
        let identity = serde_json::to_vec(&(strategy, catalog.signals(), catalog.declarations()))?;
        let digest = Sha256::digest(identity);
        Ok(Self {
            policy: format!(
                "assertion_key_{}",
                digest[..8]
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            ),
            shape: KeyShape {
                capacity: strategy.capacity.unwrap_or(MAX_ENTRIES_PER_KEY),
                preferences: lines.prefer.len(),
            },
            signals: catalog.signals().len(),
            tables,
            aggregates: compiler.aggregates,
            lines,
            progress,
            goal,
            goal_reachable,
            holds_objective,
        })
    }

    pub fn parse(text: &str, catalog: &Catalog) -> Result<Self, Box<dyn Error>> {
        Self::new(&Strategy::parse(text)?, catalog)
    }

    #[must_use]
    pub fn has_goal(&self) -> bool {
        self.goal.is_some()
    }

    #[must_use]
    pub fn policy_identifier(&self) -> &str {
        &self.policy
    }

    #[must_use]
    pub fn shape(&self) -> KeyShape {
        self.shape
    }

    #[must_use]
    pub fn signals(&self) -> usize {
        self.signals
    }

    #[must_use]
    pub fn is_pure(&self) -> bool {
        self.aggregates.is_empty()
            && match &self.progress {
                Progress::Stages { stages, .. } => stages.iter().all(|stage| !stage.latch),
                _ => true,
            }
    }

    #[must_use]
    pub fn goal_reachable(&self) -> bool {
        self.goal_reachable
    }

    #[must_use]
    pub fn stages_reachable(&self) -> Vec<bool> {
        match &self.progress {
            Progress::Stages { stages, .. } => stages.iter().map(|stage| stage.reachable).collect(),
            _ => Vec::new(),
        }
    }

    #[must_use]
    pub fn stage_names(&self) -> Vec<&str> {
        match &self.progress {
            Progress::Stages { stages, .. } => {
                stages.iter().map(|stage| stage.name.as_str()).collect()
            }
            _ => Vec::new(),
        }
    }

    fn accumulator_width(&self) -> usize {
        2 * self.aggregates.len()
            + if self.holds_objective {
                OBJECTIVE_WIDTH
            } else {
                0
            }
    }

    fn held(&self, accumulator: &Accumulator) -> Held {
        let mut held = Held {
            started: accumulator.started,
            stage: accumulator.stage,
            ..Held::default()
        };
        if accumulator.values.len() == self.accumulator_width() {
            let count = self.aggregates.len();
            let (values, rest) = accumulator.values.split_at(count);
            let (anchors, objective) = rest.split_at(count);
            held.values[..count].copy_from_slice(values);
            held.anchors[..count].copy_from_slice(anchors);
            held.objective[..objective.len()].copy_from_slice(objective);
        }
        held
    }

    fn store(&self, held: &Held, accumulator: &mut Accumulator) {
        accumulator.started = held.started;
        accumulator.stage = held.stage;
        let width = self.accumulator_width();
        if width == 0 {
            return;
        }
        if accumulator.values.len() != width {
            accumulator.values = vec![0; width].into_boxed_slice();
        }
        let count = self.aggregates.len();
        let (values, rest) = accumulator.values.split_at_mut(count);
        let (anchors, objective) = rest.split_at_mut(count);
        values.copy_from_slice(&held.values[..count]);
        anchors.copy_from_slice(&held.anchors[..count]);
        let width = objective.len();
        objective.copy_from_slice(&held.objective[..width]);
    }

    fn accumulate(&self, record: &Record, parent: &Held, stack: &mut [i64; STACK]) -> Held {
        let mut next = *parent;
        let env = Env {
            record: record.slots(),
            aggregates: &parent.values,
            locals: &parent.objective,
            tables: &self.tables,
        };
        for (index, aggregate) in self.aggregates.iter().enumerate() {
            let depth = eval(&aggregate.value, &env, stack);
            let value = stack[depth - 1];
            let anchor = aggregate.since.as_ref().map_or(0, |since| {
                let depth = eval(since, &env, stack);
                stack[depth - 1]
            });
            let reset = !parent.started || anchor != parent.anchors[index];
            let held = parent.values[index];
            next.values[index] = match (reset || held == NONE, value == NONE) {
                (true, _) => value,
                (false, true) => held,
                (false, false) => held.max(value),
            };
            next.anchors[index] = anchor;
        }
        next
    }

    #[must_use]
    pub fn observe(&self, record: &Record, accumulator: &mut Accumulator) -> Observed {
        let mut stack = [0; STACK];
        let mut next = self.held(accumulator);
        if !self.aggregates.is_empty() {
            next = self.accumulate(record, &next, &mut stack);
        }
        let mut locals = [0; OBJECTIVE_WIDTH];
        let stage = match &self.progress {
            Progress::Stages { stages, .. } => {
                let env = Env {
                    record: record.slots(),
                    aggregates: &next.values,
                    locals: &locals,
                    tables: &self.tables,
                };
                Some(Self::locate(stages, &env, &next, &mut stack))
            }
            _ => None,
        };
        if let Some((_, objective)) = stage {
            locals = objective;
        }
        let env = Env {
            record: record.slots(),
            aggregates: &next.values,
            locals: &locals,
            tables: &self.tables,
        };
        let (progress, lines) = match (&self.progress, stage) {
            (Progress::Tier(tier), _) => (tier.pack(&env, &mut stack), &self.lines),
            (Progress::Stages { stages, radix }, Some((index, objective))) => {
                let stage = &stages[usize::from(index)];
                let packed = stage.objective.lanes.iter().zip(objective).fold(
                    0_u64,
                    |packed, (lane, value)| {
                        packed
                            .wrapping_mul(lane.radix)
                            .wrapping_add(lane.pack(value))
                    },
                );
                (
                    u64::from(index) * radix + packed,
                    stage.lines.as_ref().unwrap_or(&self.lines),
                )
            }
            _ => (0, &self.lines),
        };
        let truth = |ops: &[Op], stack: &mut [i64; STACK]| {
            let depth = eval(ops, &env, stack);
            truthy(stack[depth - 1])
        };
        let absent = lines
            .absent
            .as_deref()
            .is_some_and(|ops| truth(ops, &mut stack));
        let rejected = lines
            .reject
            .iter()
            .position(|(_, ops)| truth(ops, &mut stack))
            .and_then(|index| u8::try_from(index).ok());
        let key = (!absent).then(|| {
            let mut prefer = [0; MAX_PREFERENCES];
            for (slot, tuple) in prefer.iter_mut().zip(&lines.prefer) {
                *slot = u32::try_from(tuple.pack(&env, &mut stack)).unwrap_or(u32::MAX);
            }
            AssertionKey {
                progress: u32::try_from(progress).unwrap_or(u32::MAX),
                place: Wide::new(lines.spread.pack(&env, &mut stack)),
                identity: lines.identity.pack(&env, &mut stack),
                across: u16::try_from(lines.across.pack(&env, &mut stack)).unwrap_or(u16::MAX),
                prefer,
                scope: lines.scope.as_ref().map_or_else(
                    || stage.map_or(0, |(index, _)| u16::from(index)),
                    |scope| u16::try_from(scope.pack(&env, &mut stack)).unwrap_or(u16::MAX),
                ),
            }
        });
        let goal = self
            .goal
            .as_deref()
            .is_some_and(|ops| truth(ops, &mut stack));
        if let Some((index, objective)) = stage
            && key.is_some()
        {
            next.stage = index;
            next.objective = objective;
        }
        next.started = true;
        self.store(&next, accumulator);
        Observed {
            key,
            rejected,
            goal,
        }
    }

    #[must_use]
    pub fn key(&self, record: &Record) -> Option<AssertionKey> {
        self.observe(record, &mut Accumulator::default()).key
    }

    fn locate(
        stages: &[Stage],
        env: &Env<'_>,
        parent: &Held,
        stack: &mut [i64; STACK],
    ) -> (u8, [i64; OBJECTIVE_WIDTH]) {
        let parent_stage = parent.stage();
        for (index, stage) in stages.iter().enumerate() {
            let index = u8::try_from(index).unwrap_or(u8::MAX);
            if stage.latch && parent_stage.is_some_and(|parent| parent > index) {
                continue;
            }
            let mut objective = [0; OBJECTIVE_WIDTH];
            let depth = eval(&stage.objective.ops, env, stack);
            let width = stage.objective.lanes.len();
            objective[..width].copy_from_slice(&stack[depth - width..depth]);
            let goal_env = Env {
                record: env.record,
                aggregates: env.aggregates,
                locals: &objective,
                tables: env.tables,
            };
            let depth = eval(&stage.goal, &goal_env, stack);
            if !truthy(stack[depth - 1]) {
                return (index, objective);
            }
        }
        (
            u8::try_from(stages.len().saturating_sub(1)).unwrap_or(u8::MAX),
            [0; OBJECTIVE_WIDTH],
        )
    }

    pub(super) fn stage_goals(&self, record: &Record, parent: &Accumulator) -> Vec<bool> {
        let Progress::Stages { stages, .. } = &self.progress else {
            return Vec::new();
        };
        let mut stack = [0; STACK];
        let parent = &self.held(parent);
        let next = if self.aggregates.is_empty() {
            *parent
        } else {
            self.accumulate(record, parent, &mut stack)
        };
        let none = [0; OBJECTIVE_WIDTH];
        let env = Env {
            record: record.slots(),
            aggregates: &next.values,
            locals: &none,
            tables: &self.tables,
        };
        stages
            .iter()
            .map(|stage| {
                let mut objective = [0; OBJECTIVE_WIDTH];
                let depth = eval(&stage.objective.ops, &env, &mut stack);
                let width = stage.objective.lanes.len();
                objective[..width].copy_from_slice(&stack[depth - width..depth]);
                let goal_env = Env {
                    record: env.record,
                    aggregates: env.aggregates,
                    locals: &objective,
                    tables: env.tables,
                };
                let depth = eval(&stage.goal, &goal_env, &mut stack);
                truthy(stack[depth - 1])
            })
            .collect()
    }

    #[must_use]
    pub fn reject_names(&self, stage: Option<u8>) -> Vec<&str> {
        self.lines_for(stage)
            .reject
            .iter()
            .map(|(name, _)| name.as_str())
            .collect()
    }

    fn lines_for(&self, stage: Option<u8>) -> &LineSet {
        match (&self.progress, stage) {
            (Progress::Stages { stages, .. }, Some(stage)) => stages
                .get(usize::from(stage))
                .and_then(|stage| stage.lines.as_ref())
                .unwrap_or(&self.lines),
            _ => &self.lines,
        }
    }

    #[must_use]
    pub fn view(&self, key: AssertionKey) -> KeyView {
        let (stage, progress, lines) = match &self.progress {
            Progress::Flat => (None, Vec::new(), &self.lines),
            Progress::Tier(tier) => (None, tier.unpack(u64::from(key.progress)), &self.lines),
            Progress::Stages { stages, radix } => {
                let progress = u64::from(key.progress);
                let index = usize::try_from(progress / radix).unwrap_or(usize::MAX);
                let stage = stages.get(index);
                (
                    stage.map(|stage| stage.name.clone()),
                    stage.map_or_else(Vec::new, |stage| stage.objective.unpack(progress % radix)),
                    stage
                        .and_then(|stage| stage.lines.as_ref())
                        .unwrap_or(&self.lines),
                )
            }
        };
        KeyView {
            stage,
            progress,
            place: lines.spread.unpack(key.place.get()),
            identity: lines.identity.unpack(key.identity),
        }
    }
}

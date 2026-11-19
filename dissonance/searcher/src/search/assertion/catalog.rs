// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{collections::BTreeMap, error::Error, ops::Range};

use serde::{Deserialize, Serialize};

use crate::search::choice::ChoiceSite;

pub const RECORD_SLOTS: usize = 96;
pub const ACTION_END: &str = "action_end";
pub(crate) const ACTION_END_SLOT: usize = RECORD_SLOTS - 1;
pub const EACH_FIELDS: usize = 4;
pub const NONE: i64 = i64::MIN;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Number {
    pub value: i64,
    pub min: i64,
    pub max: i64,
    pub unit: i64,
}

impl Number {
    #[must_use]
    pub const fn new(value: i64, min: i64, max: i64) -> Self {
        Self {
            value,
            min,
            max,
            unit: 1,
        }
    }

    #[must_use]
    pub const fn unit(self, unit: i64) -> Self {
        Self { unit, ..self }
    }

    #[must_use]
    pub const fn byte(value: u8) -> Self {
        Self::new(value as i64, 0, u8::MAX as i64)
    }

    #[must_use]
    pub const fn word(value: u16) -> Self {
        Self::new(value as i64, 0, u16::MAX as i64)
    }

    #[must_use]
    pub const fn flag(value: bool) -> Self {
        Self::new(value as i64, 0, 1)
    }

    #[must_use]
    pub const fn constant(value: i64) -> Self {
        Self::new(value, value, value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Assertion {
    Always {
        name: &'static str,
        condition: bool,
    },
    AlwaysOrUnreachable {
        name: &'static str,
        condition: bool,
    },
    Sometimes {
        name: &'static str,
        condition: bool,
    },
    Reachable {
        name: &'static str,
        reached: bool,
    },
    Unreachable {
        name: &'static str,
        reached: bool,
    },
    AlwaysGreaterThan {
        name: &'static str,
        left: Number,
        right: Number,
    },
    AlwaysGreaterThanOrEqualTo {
        name: &'static str,
        left: Number,
        right: Number,
    },
    AlwaysLessThan {
        name: &'static str,
        left: Number,
        right: Number,
    },
    AlwaysLessThanOrEqualTo {
        name: &'static str,
        left: Number,
        right: Number,
    },
    SometimesGreaterThan {
        name: &'static str,
        left: Number,
        right: Number,
    },
    SometimesGreaterThanOrEqualTo {
        name: &'static str,
        left: Number,
        right: Number,
    },
    SometimesLessThan {
        name: &'static str,
        left: Number,
        right: Number,
    },
    SometimesLessThanOrEqualTo {
        name: &'static str,
        left: Number,
        right: Number,
    },
    AlwaysSome {
        name: &'static str,
        terms: &'static [&'static str],
        holding: u64,
    },
    SometimesAll {
        name: &'static str,
        terms: &'static [&'static str],
        holding: u64,
    },
    SometimesEach {
        name: &'static str,
        fields: &'static [&'static str],
        values: [Number; EACH_FIELDS],
    },
    Value {
        name: &'static str,
        value: Number,
    },
    Coverage {
        name: &'static str,
        space: u32,
    },
}

impl Assertion {
    #[must_use]
    pub fn name(&self) -> &'static str {
        match *self {
            Self::Always { name, .. }
            | Self::AlwaysOrUnreachable { name, .. }
            | Self::Sometimes { name, .. }
            | Self::Reachable { name, .. }
            | Self::Unreachable { name, .. }
            | Self::AlwaysGreaterThan { name, .. }
            | Self::AlwaysGreaterThanOrEqualTo { name, .. }
            | Self::AlwaysLessThan { name, .. }
            | Self::AlwaysLessThanOrEqualTo { name, .. }
            | Self::SometimesGreaterThan { name, .. }
            | Self::SometimesGreaterThanOrEqualTo { name, .. }
            | Self::SometimesLessThan { name, .. }
            | Self::SometimesLessThanOrEqualTo { name, .. }
            | Self::AlwaysSome { name, .. }
            | Self::SometimesAll { name, .. }
            | Self::SometimesEach { name, .. }
            | Self::Value { name, .. }
            | Self::Coverage { name, .. } => name,
        }
    }

    #[must_use]
    pub fn form(&self) -> &'static str {
        match self {
            Self::Always { .. } => "always",
            Self::AlwaysOrUnreachable { .. } => "always_or_unreachable",
            Self::Sometimes { .. } => "sometimes",
            Self::Reachable { .. } => "reachable",
            Self::Unreachable { .. } => "unreachable",
            Self::AlwaysGreaterThan { .. } => "always_greater_than",
            Self::AlwaysGreaterThanOrEqualTo { .. } => "always_greater_than_or_equal_to",
            Self::AlwaysLessThan { .. } => "always_less_than",
            Self::AlwaysLessThanOrEqualTo { .. } => "always_less_than_or_equal_to",
            Self::SometimesGreaterThan { .. } => "sometimes_greater_than",
            Self::SometimesGreaterThanOrEqualTo { .. } => "sometimes_greater_than_or_equal_to",
            Self::SometimesLessThan { .. } => "sometimes_less_than",
            Self::SometimesLessThanOrEqualTo { .. } => "sometimes_less_than_or_equal_to",
            Self::AlwaysSome { .. } => "always_some",
            Self::SometimesAll { .. } => "sometimes_all",
            Self::SometimesEach { .. } => "sometimes_each",
            Self::Value { .. } => "value",
            Self::Coverage { .. } => "coverage",
        }
    }

    #[must_use]
    #[inline(always)]
    pub fn is_sometimes(&self) -> bool {
        matches!(
            self,
            Self::Sometimes { .. }
                | Self::Reachable { .. }
                | Self::SometimesGreaterThan { .. }
                | Self::SometimesGreaterThanOrEqualTo { .. }
                | Self::SometimesLessThan { .. }
                | Self::SometimesLessThanOrEqualTo { .. }
                | Self::SometimesAll { .. }
        )
    }

    #[must_use]
    #[inline(always)]
    pub fn holds(&self) -> bool {
        match *self {
            Self::Always { condition, .. }
            | Self::AlwaysOrUnreachable { condition, .. }
            | Self::Sometimes { condition, .. } => condition,
            Self::Reachable { reached, .. } => reached,
            Self::Unreachable { reached, .. } => !reached,
            Self::AlwaysGreaterThan { left, right, .. }
            | Self::SometimesGreaterThan { left, right, .. } => left.value > right.value,
            Self::AlwaysGreaterThanOrEqualTo { left, right, .. }
            | Self::SometimesGreaterThanOrEqualTo { left, right, .. } => left.value >= right.value,
            Self::AlwaysLessThan { left, right, .. }
            | Self::SometimesLessThan { left, right, .. } => left.value < right.value,
            Self::AlwaysLessThanOrEqualTo { left, right, .. }
            | Self::SometimesLessThanOrEqualTo { left, right, .. } => left.value <= right.value,
            Self::AlwaysSome { holding, .. } => holding != 0,
            Self::SometimesAll { terms, holding, .. } => {
                holding.count_ones() as usize == terms.len()
            }
            Self::SometimesEach { .. } | Self::Value { .. } | Self::Coverage { .. } => true,
        }
    }

    #[inline(always)]
    fn numbers(&self) -> Option<(Number, Number)> {
        match *self {
            Self::AlwaysGreaterThan { left, right, .. }
            | Self::AlwaysGreaterThanOrEqualTo { left, right, .. }
            | Self::AlwaysLessThan { left, right, .. }
            | Self::AlwaysLessThanOrEqualTo { left, right, .. }
            | Self::SometimesGreaterThan { left, right, .. }
            | Self::SometimesGreaterThanOrEqualTo { left, right, .. }
            | Self::SometimesLessThan { left, right, .. }
            | Self::SometimesLessThanOrEqualTo { left, right, .. } => Some((left, right)),
            _ => None,
        }
    }

    fn signals(&self) -> Vec<Signal> {
        let name = self.name();
        let form = self.form();
        let signal = |suffix: Option<&str>, min, max, unit| Signal {
            name: suffix.map_or_else(|| name.to_owned(), |suffix| format!("{name}.{suffix}")),
            form: form.to_owned(),
            min,
            max,
            unit,
        };
        if let Some((left, right)) = self.numbers() {
            return vec![
                signal(None, 0, 1, 1),
                signal(Some("left"), left.min, left.max, left.unit),
                signal(Some("right"), right.min, right.max, right.unit),
                signal(
                    Some("gap"),
                    left.min.saturating_sub(right.max),
                    left.max.saturating_sub(right.min),
                    left.unit,
                ),
            ];
        }
        match *self {
            Self::AlwaysSome { terms, .. } | Self::SometimesAll { terms, .. } => {
                let count = i64::try_from(terms.len()).unwrap_or(i64::MAX);
                std::iter::once(signal(None, 0, count, 1))
                    .chain(terms.iter().map(|term| signal(Some(term), 0, 1, 1)))
                    .collect()
            }
            Self::SometimesEach { fields, values, .. } => fields
                .iter()
                .zip(values)
                .map(|(field, value)| signal(Some(field), value.min, value.max, value.unit))
                .collect(),
            Self::Value { value, .. } => vec![signal(None, value.min, value.max, value.unit)],
            Self::Coverage { space, .. } => vec![signal(None, 0, i64::from(space), 1)],
            _ => vec![signal(None, 0, 1, 1)],
        }
    }

    #[inline(always)]
    fn write(&self, out: &mut [i64]) -> usize {
        if let Some((left, right)) = self.numbers() {
            out[0] = i64::from(self.holds());
            out[1] = left.value;
            out[2] = right.value;
            out[3] = left.value.saturating_sub(right.value);
            return 4;
        }
        match *self {
            Self::AlwaysSome { terms, holding, .. } | Self::SometimesAll { terms, holding, .. } => {
                out[0] = i64::from(holding.count_ones());
                for (index, slot) in out[1..=terms.len()].iter_mut().enumerate() {
                    *slot = i64::from((holding >> index) & 1 == 1);
                }
                terms.len() + 1
            }
            Self::SometimesEach { fields, values, .. } => {
                for (slot, value) in out.iter_mut().zip(&values[..fields.len()]) {
                    *slot = value.value;
                }
                fields.len()
            }
            Self::Value { value, .. } => {
                out[0] = value.value;
                1
            }
            Self::Coverage { .. } => {
                out[0] = 0;
                1
            }
            _ => {
                out[0] = i64::from(self.holds());
                1
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Signal {
    pub name: String,
    pub form: String,
    pub min: i64,
    pub max: i64,
    pub unit: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Declared {
    pub name: String,
    pub form: String,
    pub slots: Range<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Catalog {
    signals: Vec<Signal>,
    declarations: Vec<Declared>,
    choices: Vec<ChoiceSite>,
    #[serde(skip)]
    by_name: BTreeMap<String, usize>,
}

#[derive(Deserialize)]
struct CatalogParts {
    signals: Vec<Signal>,
    declarations: Vec<Declared>,
    choices: Vec<ChoiceSite>,
}

impl<'de> Deserialize<'de> for Catalog {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let parts = CatalogParts::deserialize(deserializer)?;
        Self::index(parts.signals, parts.declarations)
            .and_then(|catalog| catalog.with_choices(parts.choices))
            .map_err(serde::de::Error::custom)
    }
}

impl Catalog {
    pub fn from_assertions(
        assertions: impl IntoIterator<Item = Assertion>,
    ) -> Result<Self, Box<dyn Error>> {
        let mut signals = Vec::new();
        let mut declarations = Vec::new();
        for assertion in assertions {
            if let Assertion::AlwaysSome { terms, .. } | Assertion::SometimesAll { terms, .. } =
                assertion
                && terms.len() > 64
            {
                return Err(format!("{} declares more than 64 terms", assertion.name()).into());
            }
            if let Assertion::SometimesEach { fields, .. } = assertion
                && (fields.is_empty() || fields.len() > EACH_FIELDS)
            {
                return Err(format!(
                    "{} declares a tuple outside 1..={EACH_FIELDS} fields",
                    assertion.name()
                )
                .into());
            }
            let start = signals.len();
            signals.extend(assertion.signals());
            declarations.push(Declared {
                name: assertion.name().to_owned(),
                form: assertion.form().to_owned(),
                slots: start..signals.len(),
            });
        }
        if signals.len() > ACTION_END_SLOT {
            return Err(format!(
                "the catalog has {} signals and a record holds {ACTION_END_SLOT}",
                signals.len()
            )
            .into());
        }
        if signals.iter().any(|signal| signal.name == ACTION_END) {
            return Err(format!("`{ACTION_END}` is built in and no workload declares it").into());
        }
        if let Some(signal) = signals.iter().find(|signal| signal.min > signal.max) {
            return Err(format!("{} has an empty range", signal.name).into());
        }
        Self::index(signals, declarations)
    }

    fn index(signals: Vec<Signal>, declarations: Vec<Declared>) -> Result<Self, Box<dyn Error>> {
        let mut by_name = BTreeMap::new();
        for (slot, signal) in signals.iter().enumerate() {
            if by_name.insert(signal.name.clone(), slot).is_some() {
                return Err(format!("{} is declared twice", signal.name).into());
            }
        }
        for declaration in &declarations {
            if declaration.form == "sometimes_each" && by_name.contains_key(&declaration.name) {
                return Err(format!("{} is declared twice", declaration.name).into());
            }
        }
        Ok(Self {
            signals,
            declarations,
            choices: Vec::new(),
            by_name,
        })
    }

    pub fn with_choices(mut self, choices: Vec<ChoiceSite>) -> Result<Self, Box<dyn Error>> {
        if choices.len() > usize::from(u16::MAX) {
            return Err("the catalog declares more choice sites than a site id holds".into());
        }
        ChoiceSite::validate_all(&choices)?;
        let mut names = BTreeMap::new();
        for site in &choices {
            if site.labels.is_empty() || site.labels.len() > usize::from(u16::MAX) {
                return Err(format!("choice site {} declares no usable labels", site.name).into());
            }
            if names.insert(site.name.as_str(), ()).is_some() {
                return Err(format!("choice site {} is declared twice", site.name).into());
            }
            let mut labels = BTreeMap::new();
            if site
                .labels
                .iter()
                .any(|label| labels.insert(label.as_str(), ()).is_some())
            {
                return Err(format!("choice site {} repeats a label", site.name).into());
            }
        }
        self.choices = choices;
        Ok(self)
    }

    pub fn reindex(self) -> Result<Self, Box<dyn Error>> {
        let choices = self.choices;
        Self::index(self.signals, self.declarations)?.with_choices(choices)
    }

    #[must_use]
    pub fn choices(&self) -> &[ChoiceSite] {
        &self.choices
    }

    #[must_use]
    pub fn signals(&self) -> &[Signal] {
        &self.signals
    }

    #[must_use]
    pub fn declarations(&self) -> &[Declared] {
        &self.declarations
    }

    #[must_use]
    pub fn slot(&self, name: &str) -> Option<usize> {
        self.by_name.get(name).copied()
    }

    #[must_use]
    pub fn declaration(&self, name: &str) -> Option<&Declared> {
        self.declarations
            .iter()
            .find(|declaration| declaration.name == name)
    }

    #[must_use]
    pub fn group(&self, name: &str) -> Option<Range<usize>> {
        self.declaration(name)
            .filter(|declaration| declaration.form == "sometimes_each")
            .map(|declaration| declaration.slots.clone())
    }

    pub fn checked_record(
        &self,
        assertions: impl IntoIterator<Item = Assertion>,
    ) -> Result<Record, Box<dyn Error>> {
        let mut record = Record::default();
        let mut declarations = self.declarations.iter();
        for assertion in assertions {
            let declared = declarations
                .next()
                .ok_or("a record has more assertions than the catalog")?;
            if declared.name != assertion.name() || declared.form != assertion.form() {
                return Err(format!(
                    "assertion {} {} is out of catalog order",
                    assertion.form(),
                    assertion.name()
                )
                .into());
            }
            let written = assertion.write(&mut record.values[declared.slots.start..]);
            if written != declared.slots.len() {
                return Err(format!("{} changed its width", assertion.name()).into());
            }
            for slot in declared.slots.clone() {
                let signal = &self.signals[slot];
                let value = record.values[slot];
                if value < signal.min || value > signal.max {
                    return Err(format!(
                        "{} is {value}, outside its declared range {}..={}",
                        signal.name, signal.min, signal.max
                    )
                    .into());
                }
            }
        }
        if declarations.next().is_some() {
            return Err("a record has fewer assertions than the catalog".into());
        }
        record.len = u8::try_from(self.signals.len())?;
        Ok(record)
    }
}

pub trait Declarations {
    fn declare(&mut self, assertion: Assertion);
}

impl Declarations for Vec<Assertion> {
    fn declare(&mut self, assertion: Assertion) {
        self.push(assertion);
    }
}

impl Declarations for Record {
    #[inline(always)]
    fn declare(&mut self, assertion: Assertion) {
        self.push(&assertion);
    }
}

pub struct RecordWriter<'a> {
    values: &'a mut [i64; RECORD_SLOTS],
    len: &'a mut u8,
    written: usize,
}

impl RecordWriter<'_> {
    #[inline(always)]
    pub fn push(&mut self, assertion: &Assertion) {
        self.written += assertion.write(&mut self.values[self.written..]);
    }

    pub fn finish(self) {
        *self.len = u8::try_from(self.written).unwrap_or(u8::MAX);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Record {
    len: u8,
    values: [i64; RECORD_SLOTS],
}

impl Default for Record {
    fn default() -> Self {
        Self {
            len: 0,
            values: [0; RECORD_SLOTS],
        }
    }
}

impl Record {
    #[must_use]
    pub fn from_assertions(assertions: impl IntoIterator<Item = Assertion>) -> Self {
        let mut record = Self::default();
        for assertion in assertions {
            record.push(&assertion);
        }
        record
    }

    #[inline(always)]
    pub fn push(&mut self, assertion: &Assertion) {
        let slot = usize::from(self.len);
        let written = assertion.write(&mut self.values[slot..]);
        self.len = u8::try_from(slot + written).unwrap_or(u8::MAX);
    }

    pub fn writer(&mut self) -> RecordWriter<'_> {
        RecordWriter {
            values: &mut self.values,
            len: &mut self.len,
            written: 0,
        }
    }

    #[must_use]
    pub fn from_values(values: &[i64]) -> Option<Self> {
        let mut record = Self::default();
        record
            .values
            .get_mut(..values.len())?
            .copy_from_slice(values);
        record.len = u8::try_from(values.len()).ok()?;
        Some(record)
    }

    pub fn set_action_end(&mut self, end: bool) {
        self.values[ACTION_END_SLOT] = i64::from(end);
    }

    #[must_use]
    pub fn action_end(&self) -> bool {
        self.values[ACTION_END_SLOT] != 0
    }

    #[must_use]
    pub fn values(&self) -> &[i64] {
        &self.values[..usize::from(self.len)]
    }

    #[must_use]
    pub(crate) fn slots(&self) -> &[i64; RECORD_SLOTS] {
        &self.values
    }
}

impl Serialize for Record {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.values().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Record {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let values = Vec::<i64>::deserialize(deserializer)?;
        Self::from_values(&values)
            .ok_or_else(|| serde::de::Error::custom("record exceeds the slot limit"))
    }
}

// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::catalog::{Answer, DecisionPoint};
use crate::codec::{Reader, put_u16, put_u64};
use crate::error::EnvError;

pub const CHOICE_NAMESPACE: u16 = 11;

pub const MAX_CHOICE_SITE_LEN: usize = 64;

const FNV_OFFSET: u32 = 0x811c_9dc5;
const FNV_PRIME: u32 = 0x0100_0193;

const HEADER_LEN: usize = 12;

#[must_use]
pub fn choice_site(name: &str) -> u32 {
    name.bytes().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(FNV_PRIME)
    })
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ChoiceQuestion {
    pub site: String,
    pub options: u16,
    pub default: u16,
    pub clock: u64,
}

impl ChoiceQuestion {
    #[must_use]
    pub fn point(&self) -> DecisionPoint {
        DecisionPoint::Choice {
            site: choice_site(&self.site),
            options: self.options,
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, EnvError> {
        self.validate()?;
        let mut w = Vec::with_capacity(HEADER_LEN + self.site.len());
        put_u16(&mut w, self.options);
        put_u16(&mut w, self.default);
        put_u64(&mut w, self.clock);
        w.extend_from_slice(self.site.as_bytes());
        Ok(w)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, EnvError> {
        let mut r = Reader::new(bytes);
        let options = r.u16()?;
        let default = r.u16()?;
        let clock = r.u64()?;
        let site = std::str::from_utf8(&bytes[HEADER_LEN..])
            .map_err(|_| EnvError::Malformed)?
            .to_owned();
        let question = Self {
            site,
            options,
            default,
            clock,
        };
        question.validate()?;
        Ok(question)
    }

    fn validate(&self) -> Result<(), EnvError> {
        if self.options < 2
            || self.default >= self.options
            || self.site.is_empty()
            || self.site.len() > MAX_CHOICE_SITE_LEN
        {
            return Err(EnvError::Malformed);
        }
        Ok(())
    }
}

pub fn choice_answer(answer: &Answer) -> Result<Option<[u8; 2]>, EnvError> {
    match answer {
        Answer::Nominal => Ok(None),
        Answer::Label(label) => Ok(Some(label.to_le_bytes())),
        Answer::Supply(_) | Answer::Fault(_) => Err(EnvError::Malformed),
    }
}

#[cfg(kani)]
mod proofs {
    use crate::catalog::{Answer, DecisionPoint};

    #[kani::proof]
    fn a_choice_admits_the_default_and_exactly_the_labels_below_its_count() {
        let site: u32 = kani::any();
        let options: u16 = kani::any();
        let label: u16 = kani::any();
        let point = DecisionPoint::Choice { site, options };
        assert!(point.admits(&Answer::Nominal));
        assert_eq!(point.admits(&Answer::Label(label)), label < options);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question() -> ChoiceQuestion {
        ChoiceQuestion {
            site: "register.op".to_owned(),
            options: 4,
            default: 3,
            clock: 0x0a0b,
        }
    }

    #[test]
    fn a_question_round_trips_through_its_wire_form() {
        let bytes = question().encode().unwrap();
        assert_eq!(
            &bytes[..HEADER_LEN],
            &[4, 0, 3, 0, 0x0b, 0x0a, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(&bytes[HEADER_LEN..], b"register.op");
        assert_eq!(ChoiceQuestion::decode(&bytes).unwrap(), question());
    }

    #[test]
    fn malformed_questions_are_refused() {
        let mut one_option = question();
        one_option.options = 1;
        one_option.default = 0;
        assert_eq!(one_option.encode(), Err(EnvError::Malformed));

        let mut default_out_of_range = question();
        default_out_of_range.default = 4;
        assert_eq!(default_out_of_range.encode(), Err(EnvError::Malformed));

        let mut unnamed = question();
        unnamed.site.clear();
        assert_eq!(unnamed.encode(), Err(EnvError::Malformed));

        let mut long = question();
        long.site = "s".repeat(MAX_CHOICE_SITE_LEN + 1);
        assert_eq!(long.encode(), Err(EnvError::Malformed));

        let bytes = question().encode().unwrap();
        assert_eq!(
            ChoiceQuestion::decode(&bytes[..HEADER_LEN - 1]),
            Err(EnvError::Malformed)
        );
        let mut not_utf8 = bytes.clone();
        not_utf8.push(0xff);
        assert_eq!(ChoiceQuestion::decode(&not_utf8), Err(EnvError::Malformed));
    }

    #[test]
    fn site_identifiers_are_fnv_1a() {
        assert_eq!(choice_site(""), 0x811c_9dc5);
        assert_eq!(choice_site("a"), 0xe40c_292c);
        assert_eq!(
            question().point(),
            DecisionPoint::Choice {
                site: choice_site("register.op"),
                options: 4
            }
        );
    }

    #[test]
    fn only_nominal_and_label_answers_reach_the_guest() {
        assert_eq!(choice_answer(&Answer::Nominal), Ok(None));
        assert_eq!(choice_answer(&Answer::Label(0x0102)), Ok(Some([2, 1])));
        assert_eq!(
            choice_answer(&Answer::Supply(vec![0])),
            Err(EnvError::Malformed)
        );
    }

    #[test]
    fn a_label_answer_encodes_with_its_own_tag() {
        assert_eq!(Answer::Label(0x0102).encode(), vec![3, 2, 1]);
        assert_eq!(Answer::decode(&[3, 2, 1]), Ok(Answer::Label(0x0102)));
    }
}

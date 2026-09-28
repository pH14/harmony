// SPDX-License-Identifier: AGPL-3.0-or-later

use environment::channel::Answer;

trait BufferedInputs {
    fn encode_buffered(&self) -> Vec<u8>;
}

impl BufferedInputs for environment::input_spec::InputSpec {
    fn encode_buffered(&self) -> Vec<u8> {
        let mut out = b"HENV".to_vec();
        out.extend(Self::BLOB_VERSION.to_le_bytes());
        out.extend(self.seed().to_le_bytes());
        put(&mut out, &self.config().encode());
        out.extend((self.effects().len() as u32).to_le_bytes());
        for (at, effect) in self.effects() {
            out.extend(at.to_le_bytes());
            put(&mut out, &effect.encode());
        }
        out.extend((self.reseeds().len() as u32).to_le_bytes());
        for (at, seed) in self.reseeds() {
            out.extend(at.to_le_bytes());
            out.extend(seed.to_le_bytes());
        }
        match self.payloads() {
            None => out.push(0),
            Some(payloads) => {
                out.push(1);
                out.extend((payloads.len() as u32).to_le_bytes());
                for p in payloads {
                    put(&mut out, p);
                }
            }
        }
        out.extend((self.answers().len() as u32).to_le_bytes());
        for ((at, service, request), answer) in self.answers() {
            out.extend(at.to_le_bytes());
            out.extend(service.to_le_bytes());
            out.extend(request.to_le_bytes());
            match answer {
                Answer::Nominal => out.push(0),
                Answer::Data(bytes) => {
                    out.push(1);
                    put(&mut out, bytes);
                }
            }
        }
        out
    }
}

fn put(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend((bytes.len() as u32).to_le_bytes());
    out.extend(bytes);
}

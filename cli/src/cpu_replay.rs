// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CpuIdentity {
    #[serde(default)]
    pub architecture: String,
    #[serde(default)]
    pub implementations: BTreeSet<BTreeMap<String, String>>,
    #[serde(default)]
    pub complete: bool,
    #[serde(default)]
    pub hybrid: bool,
}

#[cfg(any(target_os = "linux", test))]
const LINUX_X86: &[&str] = &[
    "vendor_id",
    "cpu family",
    "model",
    "stepping",
    "microcode",
    "flags",
];
#[cfg(any(target_os = "linux", test))]
const LINUX_ARM: &[&str] = &[
    "CPU implementer",
    "CPU architecture",
    "CPU variant",
    "CPU part",
    "CPU revision",
    "Features",
];
#[cfg(any(target_os = "macos", test))]
const MAC_COMMON: &[&str] = &[
    "machdep.cpu.brand_string",
    "hw.cpufamily",
    "hw.cputype",
    "hw.cpusubtype",
    "hw.model",
    "hw.nperflevels",
];
#[cfg(any(target_os = "macos", test))]
const MAC_X86: &[&str] = &[
    "machdep.cpu.vendor",
    "machdep.cpu.family",
    "machdep.cpu.model",
    "machdep.cpu.stepping",
    "machdep.cpu.microcode_version",
    "machdep.cpu.features",
    "machdep.cpu.extfeatures",
    "machdep.cpu.leaf7_features",
];

impl CpuIdentity {
    pub fn unknown(architecture: &str) -> Self {
        Self {
            architecture: architecture.to_owned(),
            implementations: BTreeSet::new(),
            complete: false,
            hybrid: false,
        }
    }

    pub fn current() -> Self {
        let architecture = std::env::consts::ARCH;
        #[cfg(miri)]
        {
            Self::unknown(architecture)
        }
        #[cfg(all(not(miri), target_os = "linux"))]
        {
            let identity = std::fs::read_to_string("/proc/cpuinfo")
                .map(|text| parse_linux(architecture, &text))
                .unwrap_or_else(|_| Self::unknown(architecture));
            #[cfg(target_arch = "x86_64")]
            {
                let maximum = std::arch::x86_64::__cpuid(0).eax;
                let hybrid =
                    maximum >= 7 && std::arch::x86_64::__cpuid_count(7, 0).edx & (1 << 15) != 0;
                identity.with_hybrid_hint(hybrid)
            }
            #[cfg(not(target_arch = "x86_64"))]
            {
                identity
            }
        }
        #[cfg(all(not(miri), target_os = "macos"))]
        {
            let mut fields = MAC_COMMON.to_vec();
            match architecture {
                "x86_64" => fields.extend_from_slice(MAC_X86),
                "aarch64" => fields.push("hw.optional.arm"),
                _ => return Self::unknown(architecture),
            }
            match std::process::Command::new("/usr/sbin/sysctl")
                .args(fields)
                .stderr(std::process::Stdio::null())
                .output()
            {
                Ok(output) => match std::str::from_utf8(&output.stdout) {
                    Ok(text) => {
                        let mut identity = parse_macos(architecture, text);
                        identity.complete &= output.status.success();
                        identity
                    }
                    Err(_) => Self::unknown(architecture),
                },
                Err(_) => Self::unknown(architecture),
            }
        }
        #[cfg(all(not(miri), not(any(target_os = "linux", target_os = "macos"))))]
        {
            Self::unknown(architecture)
        }
    }

    #[cfg(any(all(target_os = "linux", target_arch = "x86_64", not(miri)), test))]
    fn with_hybrid_hint(mut self, hybrid: bool) -> Self {
        self.hybrid = hybrid;
        self.complete &= !hybrid;
        self
    }

    fn hybrid_topology(&self) -> bool {
        self.hybrid
            || self.implementations.iter().any(|fields| {
                fields
                    .get("hw.nperflevels")
                    .and_then(|levels| levels.parse::<u64>().ok())
                    .is_some_and(|levels| levels > 1)
            })
    }

    fn heterogeneous(&self) -> bool {
        self.implementations.len() > 1 || self.hybrid_topology()
    }
}

pub fn warning(recorded: &[CpuIdentity], current: &CpuIdentity) -> Option<String> {
    let reason = if current.heterogeneous() || recorded.iter().any(CpuIdentity::heterogeneous) {
        "the observed host has multiple CPU implementation classes"
    } else if recorded.is_empty()
        || !current.complete
        || current.architecture.is_empty()
        || current.implementations.is_empty()
        || current.implementations.iter().any(BTreeMap::is_empty)
        || recorded.iter().any(|identity| {
            !identity.complete
                || identity.architecture.is_empty()
                || identity.implementations.is_empty()
                || identity.implementations.iter().any(BTreeMap::is_empty)
        })
    {
        "the recorded or current CPU identity is missing or incomplete"
    } else if recorded
        .iter()
        .any(|identity| identity.architecture != current.architecture)
    {
        "the CPU architecture differs from the recorded execution"
    } else if recorded.iter().any(|identity| identity != current) {
        "the CPU implementation, microcode, or features differ from the recorded execution"
    } else {
        return None;
    };
    Some(format!(
        "Replay CPU portability warning: {reason}. Native CPU behavior may produce different result bits or saved state. This warning does not block execution; cross-CPU whole-state identity is not qualified."
    ))
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn normalized(key: &str, value: &str) -> String {
    if matches!(key, "flags" | "Features") || key.ends_with("features") {
        value
            .split_whitespace()
            .map(str::to_ascii_lowercase)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        value.split_whitespace().collect::<Vec<_>>().join(" ")
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn insert(fields: &mut BTreeMap<String, String>, key: &str, value: &str) -> bool {
    let value = normalized(key, value);
    if value.is_empty() {
        return false;
    }
    fields
        .insert(key.to_owned(), value.clone())
        .is_none_or(|previous| previous == value)
}

#[cfg(any(target_os = "linux", test))]
fn parse_linux(architecture: &str, text: &str) -> CpuIdentity {
    let required = match architecture {
        "x86_64" => LINUX_X86,
        "aarch64" => LINUX_ARM,
        _ => return CpuIdentity::unknown(architecture),
    };
    let mut identity = CpuIdentity::unknown(architecture);
    let mut complete = true;
    let mut observed = false;
    for block in text.split("\n\n") {
        let mut fields = BTreeMap::new();
        let mut cpu_block = false;
        let mut consistent = true;
        for (key, value) in block.lines().filter_map(|line| line.split_once(':')) {
            let key = key.trim();
            cpu_block |= key == "processor";
            if required.contains(&key) {
                consistent &= insert(&mut fields, key, value);
            }
        }
        if !cpu_block && fields.is_empty() {
            continue;
        }
        observed = true;
        complete &= cpu_block && consistent && required.iter().all(|key| fields.contains_key(*key));
        if !fields.is_empty() {
            identity.implementations.insert(fields);
        }
    }
    identity.complete = observed && complete;
    identity
}

#[cfg(any(target_os = "macos", test))]
fn parse_macos(architecture: &str, text: &str) -> CpuIdentity {
    if !matches!(architecture, "x86_64" | "aarch64") {
        return CpuIdentity::unknown(architecture);
    }
    let mut fields = BTreeMap::new();
    let mut consistent = true;
    for (key, value) in text.lines().filter_map(|line| line.split_once(':')) {
        let key = key.trim();
        if MAC_COMMON.contains(&key)
            || (architecture == "x86_64" && MAC_X86.contains(&key))
            || (architecture == "aarch64" && key.starts_with("hw.optional.arm."))
        {
            consistent &= insert(&mut fields, key, value);
        }
    }
    let complete = consistent
        && fields
            .get("hw.nperflevels")
            .is_some_and(|levels| levels == "1")
        && MAC_COMMON.iter().all(|key| fields.contains_key(*key))
        && if architecture == "x86_64" {
            MAC_X86.iter().all(|key| fields.contains_key(*key))
        } else {
            fields.keys().any(|key| key.starts_with("hw.optional.arm."))
        };
    CpuIdentity {
        architecture: architecture.to_owned(),
        implementations: if fields.is_empty() {
            BTreeSet::new()
        } else {
            BTreeSet::from([fields])
        },
        complete,
        hybrid: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linux_x86() -> &'static str {
        "processor: 0\nvendor_id: GenuineIntel\ncpu family: 6\nmodel: 183\nstepping: 1\nmicrocode: 0x12b\nflags: sse2 avx fpu\n"
    }

    fn mac_x86() -> &'static str {
        "machdep.cpu.brand_string: Example CPU\nhw.cpufamily: 1\nhw.cputype: 2\nhw.cpusubtype: 3\nhw.model: ExampleBoard\nhw.nperflevels: 1\nmachdep.cpu.vendor: GenuineIntel\nmachdep.cpu.family: 6\nmachdep.cpu.model: 183\nmachdep.cpu.stepping: 1\nmachdep.cpu.microcode_version: 299\nmachdep.cpu.features: SSE2 AVX FPU\nmachdep.cpu.extfeatures: SYSCALL XD\nmachdep.cpu.leaf7_features: BMI1 BMI2\n"
    }

    #[cfg(miri)]
    #[test]
    fn miri_current_identity_uses_no_host_observations() {
        assert_eq!(
            CpuIdentity::current(),
            CpuIdentity::unknown(std::env::consts::ARCH)
        );
    }

    #[test]
    fn unchanged_complete_single_class_has_no_warning() {
        let identity = parse_linux("x86_64", linux_x86());
        assert!(identity.complete);
        assert!(warning(&[identity.clone(), identity.clone()], &identity).is_none());
    }

    #[test]
    fn vendor_model_microcode_and_features_each_trigger_a_warning() {
        let identity = parse_linux("x86_64", linux_x86());
        for (from, to) in [
            ("GenuineIntel", "AuthenticAMD"),
            ("model: 183", "model: 184"),
            ("0x12b", "0x12c"),
            ("avx", "avx2"),
        ] {
            let changed = parse_linux("x86_64", &linux_x86().replace(from, to));
            assert!(changed.complete);
            let message = warning(std::slice::from_ref(&identity), &changed).unwrap();
            assert!(message.contains("different result bits"));
            assert!(message.contains("or saved state"));
            assert!(message.contains("This warning does not block execution"));
        }
    }

    #[test]
    fn cpu_field_and_feature_order_and_processor_ids_are_ignored() {
        let original = parse_linux("x86_64", linux_x86());
        let reordered = linux_x86()
            .replace("processor: 0", "processor: 99")
            .replace("sse2 avx fpu", "FPU sse2 avx avx")
            .lines()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(parse_linux("x86_64", &reordered), original);
        let repeated = format!(
            "{}\n{}",
            linux_x86(),
            linux_x86().replace("processor: 0", "processor: 12")
        );
        assert_eq!(parse_linux("x86_64", &repeated), original);
    }

    #[test]
    fn missing_fields_and_empty_legacy_identity_warn() {
        let complete = parse_linux("x86_64", linux_x86());
        let partial = parse_linux("x86_64", &linux_x86().replace("microcode: 0x12b\n", ""));
        assert!(!partial.complete);
        assert!(warning(std::slice::from_ref(&complete), &partial).is_some());
        assert!(warning(&[], &complete).is_some());
        assert!(warning(&[CpuIdentity::unknown("x86_64")], &complete).is_some());
        assert!(!parse_linux("x86_64", "processor: 0\n").complete);
    }

    #[test]
    fn heterogeneous_implementations_warn_even_when_the_observed_set_matches() {
        let text = format!(
            "{}\n{}",
            linux_x86(),
            linux_x86().replace("model: 183", "model: 184")
        );
        let mixed = parse_linux("x86_64", &text);
        assert!(mixed.complete);
        assert_eq!(mixed.implementations.len(), 2);
        assert!(
            warning(std::slice::from_ref(&mixed), &mixed)
                .unwrap()
                .contains("multiple CPU implementation classes")
        );
    }

    #[test]
    fn linux_cpuid_hybrid_hint_warns_even_when_all_cpu_identity_fields_match() {
        let text = linux_x86();
        let identity = parse_linux(
            "x86_64",
            &format!("{text}\n{}", text.replace("processor: 0", "processor: 1")),
        )
        .with_hybrid_hint(true);
        assert_eq!(identity.implementations.len(), 1);
        assert!(identity.hybrid);
        assert!(!identity.complete);
        let message = warning(std::slice::from_ref(&identity), &identity).unwrap();
        assert!(message.contains("multiple CPU implementation classes"));
        assert!(!message.contains("missing or incomplete"));
        assert!(
            warning(&[], &identity)
                .unwrap()
                .contains("multiple CPU implementation classes")
        );
        let homogeneous = parse_linux("x86_64", linux_x86());
        assert!(
            warning(&[identity], &homogeneous)
                .unwrap()
                .contains("multiple CPU implementation classes")
        );
    }

    #[test]
    fn incomplete_secondary_cpu_cannot_be_hidden_by_a_complete_primary() {
        let identity = parse_linux("x86_64", &format!("{}\nprocessor: 1\n", linux_x86()));
        assert_eq!(identity.implementations.len(), 1);
        assert!(!identity.complete);
    }

    #[test]
    fn arm_identity_uses_implementation_fields_and_ignores_serials() {
        let text = "processor: 0\nCPU implementer: 0x41\nCPU architecture: 8\nCPU variant: 0x0\nCPU part: 0xd03\nCPU revision: 4\nFeatures: fp asimd aes\nSerial: private-value\n";
        let identity = parse_linux("aarch64", text);
        assert!(identity.complete);
        assert_eq!(
            identity,
            parse_linux(
                "aarch64",
                &text
                    .replace("private-value", "other-value")
                    .replace("fp asimd aes", "aes fp asimd")
            )
        );
        assert!(!format!("{identity:?}").contains("private-value"));
    }

    #[test]
    fn mac_fields_are_whitelisted_and_feature_order_is_canonical() {
        let original = parse_macos("x86_64", mac_x86());
        assert!(original.complete);
        let reordered = mac_x86().replace("SSE2 AVX FPU", "FPU SSE2 AVX SSE2");
        assert_eq!(
            original,
            parse_macos("x86_64", &(reordered + "hw.serialnumber: private-value\n"))
        );
        assert!(!format!("{original:?}").contains("private-value"));
        assert!(
            !parse_macos(
                "x86_64",
                &mac_x86().replace("machdep.cpu.microcode_version: 299\n", "")
            )
            .complete
        );
    }

    #[test]
    fn mac_arm_identity_requires_observed_feature_fields() {
        let text = "machdep.cpu.brand_string: Example ARM\nhw.cpufamily: 1\nhw.cputype: 2\nhw.cpusubtype: 3\nhw.model: ExampleBoard\nhw.nperflevels: 1\nhw.optional.arm.FEAT_AES: 1\n";
        assert!(parse_macos("aarch64", text).complete);
        assert!(
            !parse_macos(
                "aarch64",
                &text.replace("hw.optional.arm.FEAT_AES: 1\n", "")
            )
            .complete
        );
    }

    #[test]
    fn mac_hybrid_hosts_remain_incomplete_even_with_the_same_aggregate_identity() {
        let identity = parse_macos(
            "x86_64",
            &mac_x86().replace("hw.nperflevels: 1", "hw.nperflevels: 2"),
        );
        assert!(!identity.complete);
        let message = warning(std::slice::from_ref(&identity), &identity).unwrap();
        assert!(message.contains("multiple CPU implementation classes"));
        assert!(!message.contains("missing or incomplete"));
    }

    #[test]
    fn unknown_identity_keeps_the_missing_identity_warning_reason() {
        let unknown = CpuIdentity::unknown("x86_64");
        let message = warning(std::slice::from_ref(&unknown), &unknown).unwrap();
        assert!(message.contains("missing or incomplete"));
        assert!(!message.contains("multiple CPU implementation classes"));
    }

    #[test]
    fn recorded_cpu_history_and_architecture_changes_are_checked() {
        let current = parse_linux("x86_64", linux_x86());
        let earlier = parse_linux("x86_64", &linux_x86().replace("model: 183", "model: 184"));
        assert!(warning(&[earlier, current.clone()], &current).is_some());
        let mut other_architecture = current.clone();
        other_architecture.architecture = "aarch64".to_owned();
        assert!(
            warning(&[other_architecture], &current)
                .unwrap()
                .contains("architecture differs")
        );
    }

    #[test]
    fn missing_serialized_identity_fields_remain_unknown_and_known_fields_round_trip() {
        let partial: CpuIdentity = serde_json::from_str(r#"{"architecture":"x86_64"}"#).unwrap();
        let current = parse_linux("x86_64", linux_x86());
        assert_eq!(partial, CpuIdentity::unknown("x86_64"));
        assert!(warning(&[partial], &current).is_some());
        let encoded = serde_json::to_vec(&current).unwrap();
        assert_eq!(
            serde_json::from_slice::<CpuIdentity>(&encoded).unwrap(),
            current
        );
        let mut legacy = serde_json::to_value(&current).unwrap();
        legacy.as_object_mut().unwrap().remove("hybrid");
        assert_eq!(
            serde_json::from_value::<CpuIdentity>(legacy).unwrap(),
            current
        );
        let hybrid = current.with_hybrid_hint(true);
        let encoded = serde_json::to_vec(&hybrid).unwrap();
        let decoded = serde_json::from_slice::<CpuIdentity>(&encoded).unwrap();
        assert_eq!(decoded, hybrid);
        assert!(warning(std::slice::from_ref(&decoded), &decoded).is_some());
    }

    #[test]
    fn conflicting_duplicate_fields_are_incomplete() {
        let original = parse_linux("x86_64", linux_x86());
        assert!(!parse_linux("x86_64", &(linux_x86().to_owned() + "model: 184\n")).complete);
        assert!(!parse_macos("x86_64", &(mac_x86().to_owned() + "hw.cpufamily: 2\n")).complete);
        assert!(warning(&[original], &CpuIdentity::unknown("unknown")).is_some());
    }
}

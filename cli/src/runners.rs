// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{
    config::{Backend, Result, Runner},
    runtime::Consonance,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Quicknes {
    pub core: Option<PathBuf>,
}
pub fn consonance(spec: &Runner) -> Result<Consonance> {
    let mut options = spec.options.clone();
    if let Some(backend) = &spec.backend {
        options.insert("backend".into(), backend.clone().into());
    }
    let runtime: Consonance = toml::Value::Table(options).try_into()?;
    if runtime.ram_mib == 0 {
        return Err("runner.options.ram_mib must be positive".into());
    }
    if runtime.uml_profile.is_some() && !matches!(runtime.backend, Backend::Auto | Backend::Uml) {
        return Err("uml_profile requires backend uml or auto".into());
    }
    Ok(runtime)
}
pub fn store_consonance(spec: &mut Runner, runtime: &Consonance) -> Result<()> {
    let mut options = toml::Value::try_from(runtime)?
        .as_table()
        .ok_or("invalid runner options")?
        .clone();
    spec.backend = options
        .remove("backend")
        .and_then(|v| v.as_str().map(str::to_owned));
    spec.kind = "consonance".into();
    spec.options = options;
    Ok(())
}
pub fn normalize(spec: &mut Runner, base: &Path) -> Result<()> {
    match spec.kind.as_str() {
        "" => Ok(()),
        "consonance" => {
            let mut c = consonance(spec)?;
            for path in [&mut c.kernel, &mut c.base_initramfs, &mut c.uml_profile] {
                crate::config::resolve_path(path, base);
            }
            store_consonance(spec, &c)
        }
        "quicknes" => {
            if spec.backend.is_some() {
                return Err("quicknes has no virtualization backend; choose runner consonance for --backend".into());
            }
            let mut c: Quicknes = toml::Value::Table(spec.options.clone()).try_into()?;
            crate::config::resolve_path(&mut c.core, base);
            spec.options = toml::Value::try_from(c)?
                .as_table()
                .ok_or("invalid runner options")?
                .clone();
            Ok(())
        }
        #[cfg(test)]
        "test" => Ok(()),
        other => Err(format!("unknown runner {other:?}").into()),
    }
}
pub fn resolve(spec: &mut Runner, offline: bool) -> Result<()> {
    match spec.kind.as_str() {
        "consonance" => {
            let mut c = consonance(spec)?;
            crate::runtime::resolve(&mut c, offline)?;
            store_consonance(spec, &c)
        }
        "quicknes" => {
            let c: Quicknes = toml::Value::Table(spec.options.clone()).try_into()?;
            let core = c
                .core
                .ok_or("set runner.options.core to the QuickNES library")?;
            if !core.is_file() {
                return Err(format!("runner artifact {} does not exist", core.display()).into());
            }
            eprintln!("runner: quicknes");
            Ok(())
        }
        #[cfg(test)]
        "test" => Ok(()),
        other => Err(format!("unknown runner {other:?}").into()),
    }
}
pub fn identity(spec: &Runner) -> Result<serde_json::Value> {
    Ok(match spec.kind.as_str() {
        "consonance" => {
            let c = consonance(spec)?;
            let host = if c.backend == Backend::Uml {
                serde_json::to_value(uml_host_identity()?)?
            } else {
                serde_json::Value::Null
            };
            serde_json::json!({"kind":spec.kind,"backend":c.backend,"host":host})
        }
        "quicknes" => serde_json::json!({"kind":"quicknes"}),
        #[cfg(test)]
        "test" => serde_json::json!({"kind":"test"}),
        other => return Err(format!("unknown runner {other:?}").into()),
    })
}

fn uml_host_identity() -> Result<uml::HostIdentity> {
    #[cfg(target_os = "linux")]
    {
        let pool =
            consonance_client::placement::CorePool::detect().map_err(std::io::Error::other)?;
        let processor = pool
            .plan(1)
            .map_err(std::io::Error::other)?
            .coordinator
            .ok_or_else(|| {
                std::io::Error::other("User-mode Linux needs a coordinator processor")
            })?;
        Ok(uml::HostIdentity::for_processor(processor)?)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(uml::HostIdentity::current()?)
    }
}
pub fn verify(spec: &Runner, expected: &serde_json::Value) -> Result<()> {
    if spec.kind == "consonance" {
        let c = consonance(spec)?;
        if c.backend == Backend::Auto {
            return Err("recordingner must pin its backend".into());
        }
        crate::runtime::choose(&c)?;
    }
    if &identity(spec)? != expected {
        return Err("runner identity differs from the recording".into());
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "linux")]
    #[cfg_attr(miri, ignore)]
    fn uml_manifest_uses_the_campaign_coordinator_processor() {
        let spec = Runner {
            kind: "consonance".into(),
            backend: Some("uml".into()),
            options: toml::Table::new(),
        };
        let expected = uml_host_identity().unwrap();
        let value = identity(&spec).unwrap();
        assert_eq!(value["host"], serde_json::to_value(expected).unwrap());
        verify(&spec, &value).unwrap();

        let mut changed = value;
        changed["host"]["processor"] = serde_json::json!(usize::MAX);
        assert!(verify(&spec, &changed).is_err());
    }
}

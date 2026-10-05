// SPDX-License-Identifier: AGPL-3.0-or-later

use super::config::{Config, Language, Result};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

pub fn sdk(offline: bool) -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("HARMONY_SDK_DIR") {
        return Ok(PathBuf::from(path).canonicalize()?);
    }
    let local = Path::new("workloads/languages");
    if local.is_dir() {
        return Ok(std::env::current_dir()?);
    }
    Ok(crate::runtime::acquire("sdk", std::env::consts::ARCH, offline)?.join("sdk"))
}

pub fn run(c: &mut Config, offline: bool) -> Result<serde_json::Value> {
    c.validate()?;
    let image = c
        .image
        .clone()
        .ok_or("set image to the output image name in harmony.toml")?;
    let context = c.build.context.clone().unwrap_or(std::env::current_dir()?);
    if let Some(dockerfile) = &c.build.dockerfile {
        checked(
            Command::new(container_tool()?)
                .args(["build", "--file"])
                .arg(dockerfile)
                .arg("--tag")
                .arg(&image)
                .arg(&context),
        )?;
    } else if let Some(language) = c.build.language {
        container_tool()?;
        let sdk = sdk(offline)?;
        let language_name = language.name();
        checked(
            Command::new("bash")
                .arg(sdk.join("workloads/languages/build-image.sh"))
                .arg(language_name),
        )?;
        let builder = format!("harmony-language-{language_name}-builder:local");
        if matches!(
            language,
            Language::C | Language::Rust | Language::Go | Language::Python
        ) {
            checked(
                Command::new(container_tool()?)
                    .arg("build")
                    .args(["--target", "compiled", "--tag"])
                    .arg(&builder)
                    .arg("--file")
                    .arg(sdk.join(format!("workloads/languages/{language_name}/Dockerfile")))
                    .arg(&sdk),
            )?;
        }
        let recipe = recipe(language, &context, &c.build.command)?;
        let temp = tempfile::tempdir()?;
        let dockerfile = temp.path().join("Dockerfile");
        fs::write(&dockerfile, recipe)?;
        let ignore = temp.path().join("Dockerfile.dockerignore");
        let inherited = fs::read_to_string(context.join(".dockerignore")).unwrap_or_default();
        fs::write(&ignore, format!("{inherited}\n.git\n.harmony\n"))?;
        let tool = container_tool()?;
        let mut build = Command::new(tool);
        build
            .args(["build", "--file"])
            .arg(&dockerfile)
            .arg("--tag")
            .arg(&image);
        if tool == "podman" {
            build.arg("--ignorefile").arg(&ignore);
        }
        checked(build.arg(&context))?;
    } else if !c.build.command.is_empty() {
        let mut command = Command::new(&c.build.command[0]);
        command.args(&c.build.command[1..]).current_dir(&context);
        checked(&mut command)?;
    }
    let admission = faults_workload::admission::inspect_image(&image)?;
    admission.require_admission()?;
    let bundle = c.bundle()?;
    let report = serde_json::json!({ "image": image, "admission": admission, "bundle": bundle,
        "language": c.build.language, "next": "harmony doctor, then harmony search" });
    Ok(report)
}

fn container_tool() -> Result<&'static str> {
    for tool in ["docker", "podman"] {
        if Command::new(tool)
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success())
        {
            return Ok(tool);
        }
    }
    Err("preparation requires Docker or Podman".into())
}

fn checked(command: &mut Command) -> Result<()> {
    if let Ok(tool) = container_tool() {
        command.env("HARMONY_CONTAINER_TOOL", tool);
    }
    command.env("BUILDAH_FORMAT", "docker");
    command.stdout(std::process::Stdio::from(std::io::stderr()));
    if !command.status()?.success() {
        return Err("preparation command failed; see its output above".into());
    }
    Ok(())
}

fn recipe(language: Language, context: &Path, command: &[String]) -> Result<String> {
    let name = language.name();
    let runtime = format!("harmony-language-{name}:local");
    if matches!(language, Language::Python | Language::Java) {
        if !command.is_empty() {
            return Err("Python and Java custom builds use build.dockerfile; their language images provide the runtime".into());
        }
        if language == Language::Python {
            return Ok(format!(
                "FROM harmony-language-python-builder:local AS application\nWORKDIR /app\nCOPY . /app\nRUN /opt/python/bin/python3.14 /build/recipe/coverage_edges.py /app harmony-application /out/application.sym.tsv && /opt/python/bin/python3.14 -m compileall -q -f --invalidation-mode checked-hash /app\nFROM {runtime}\nWORKDIR /app\nCOPY --from=application /app /app\nCOPY --from=application /out/application.sym.tsv /symbols/application.sym.tsv\nCMD [\"/opt/python/bin/python3.14\", \"/app/main.py\"]\n"
            ));
        }
        return Ok(format!(
            "FROM {runtime}\nWORKDIR /app\nCOPY . /app\nCMD [\"/opt/java/bin/java\", \"-jar\", \"/app/app.jar\"]\n"
        ));
    }
    let build = if !command.is_empty() {
        format!("RUN {}\n", serde_json::to_string(command)?)
    } else {
        match language {
            Language::C => "RUN clang -O1 -g -fsanitize-coverage=trace-pc-guard -c main.c -o /out/application.o && clang /out/application.o /build/shim.o -pthread -ldl -Wl,--build-id -o /out/application\n".into(),
            Language::Rust => {
                let manifest: toml::Value = toml::from_str(&fs::read_to_string(context.join("Cargo.toml"))?)?;
                let binary = manifest.get("package").and_then(|v| v.get("name")).and_then(toml::Value::as_str).ok_or("Rust preparation needs a package Cargo.toml; use build.dockerfile for workspaces")?;
                if !binary.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)) { return Err("invalid Rust package name".into()); }
                format!("RUN cargo add antithesis-instrumentation@=0.1.0 && printf '\\nuse antithesis_instrumentation as _;\\n' >> src/main.rs && target=\"$(rustc -vV | sed -n 's/^host: //p')\" && cargo build --release --locked --target \"$target\" && cp \"target/$target/release/{binary}\" /out/application\n")
            },
            Language::Go => "RUN mkdir -p /out/app-symbols /out/app-stdlib && bash /build/configure-stdlib.sh /build/sdk \"$(go list -m)\" /out/app-stdlib > /out/app-stdlib/instrumentation.env && source /out/app-stdlib/instrumentation.env && ANTITHESIS_SDK_MODULE_DIR=/build/sdk ANTITHESIS_SYMBOLS_DIR=/out/app-symbols ANTITHESIS_SYMBOL_PREFIX=application go build -toolexec=antithesis-go-toolexec -buildvcs=false -o /out/application .\n".into(),
            _ => unreachable!(),
        }
    };
    let go_symbols = if language == Language::Go {
        "COPY --from=application /out/app-symbols/ /symbols/\nCOPY --from=application /out/app-stdlib/ /symbols/stdlib/\n"
    } else {
        ""
    };
    Ok(format!("FROM harmony-language-{name}-builder:local AS application\nWORKDIR /app\nCOPY . /app\n{build}RUN mkdir -p /out/app-symbols && cp /out/application /out/app-symbols/application && nm --defined-only /out/application | awk '$2 ~ /[tT]/ {{print $1 \\\"\\t\\\" $3}}' > /out/app-symbols/native.sym.tsv && sha256sum /out/application | awk '{{print $1 \\\"  /opt/harmony/application\\\"}}' > /out/app-attestation\nFROM {runtime}\nCOPY --from=application /out/application /opt/harmony/application\nCOPY --from=application /out/app-symbols/ /symbols/\n{go_symbols}COPY --from=application /out/app-attestation /tmp/app-attestation\nRUN cat /tmp/app-attestation >> /symbols/harmony-instrumented-events && rm /tmp/app-attestation\nCMD [\\\"/opt/harmony/application\\\"]\n").replace("\\\"", "\""))
}

pub fn init(path: &Path, language: Option<Language>, image: Option<String>) -> Result<()> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let app_command = match language {
        Some(Language::Python) => "['/opt/python/bin/python3.14', '/app/main.py']",
        Some(Language::Java) => "['/opt/java/bin/java', '-jar', '/app/app.jar']",
        _ => "['/opt/harmony/application']",
    };
    let language = language
        .map(|language| {
            format!(
                "\n[build]\nlanguage = {:?}\ncontext = '.'\n",
                language.name()
            )
        })
        .unwrap_or_default();
    let image = image.unwrap_or_else(|| "harmony-app:local".into());
    let text = format!(
        "image = {}\nexecutions = 1000\nram_mib = 1024\n{language}\n[nodes.app]\ncommand = {app_command}\n",
        toml::Value::String(image)
    );
    let legacy: Config = toml::from_str(&text)?;
    let mut shared = legacy.shared()?;
    shared.workload.options.retain(|_, value| match value {
        toml::Value::Array(values) => !values.is_empty(),
        toml::Value::Table(values) => !values.is_empty(),
        _ => true,
    });
    let text = toml::to_string_pretty(&shared)?;
    file.write_all(text.as_bytes())?;
    Ok(())
}

pub fn language(explicit: Option<String>, directory: &Path) -> Result<Option<Language>> {
    if let Some(value) = explicit {
        return Ok(Some(toml::Value::String(value).try_into()?));
    }
    let candidates = [
        ("Cargo.toml", Language::Rust),
        ("go.mod", Language::Go),
        ("main.py", Language::Python),
        ("pyproject.toml", Language::Python),
        ("app.jar", Language::Java),
        ("pom.xml", Language::Java),
        ("build.gradle", Language::Java),
        ("main.c", Language::C),
    ];
    let mut found = Vec::new();
    for (file, language) in candidates {
        if directory.join(file).exists()
            && !found.iter().any(|l: &Language| l.name() == language.name())
        {
            found.push(language);
        }
    }
    match found.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(*one)),
        _ => Err("multiple application languages detected; choose --language".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn initialization_is_parseable_and_does_not_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("harmony.toml");
        init(&path, Some(Language::C), None).unwrap();
        let config: crate::config::Config =
            toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        config.validate().unwrap();
        assert!(init(&path, None, None).is_err());
    }
}

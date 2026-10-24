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
    if let Some(checkout) = crate::runtime::checkout()
        && checkout.join("workloads/languages").is_dir()
    {
        return Ok(checkout);
    }
    Ok(crate::runtime::acquire("sdk", std::env::consts::ARCH, offline)?.join("sdk"))
}

pub fn run(c: &mut Config, offline: bool) -> Result<serde_json::Value> {
    c.validate()?;
    let mode = super::requested_execution_mode(c)?;
    let image = c
        .image
        .clone()
        .ok_or("set image to the output image name in harmony.toml")?;
    let context = c.build.context.clone().unwrap_or(std::env::current_dir()?);
    if let Some(dockerfile) = &c.build.dockerfile {
        let tool = container_tool()?;
        let mut build = Command::new(tool);
        build
            .args(["build", "--file"])
            .arg(dockerfile)
            .arg("--tag")
            .arg(&image);
        if tool == "podman" {
            build.args(["--format", "docker"]);
        }
        for (name, value) in &c.build.args {
            build.arg("--build-arg").arg(format!("{name}={value}"));
        }
        if !c.build.args.contains_key("HARMONY_RUNTIME_IMAGE")
            && declares_runtime_image(&fs::read_to_string(dockerfile)?)
        {
            build
                .arg("--build-arg")
                .arg(format!("HARMONY_RUNTIME_IMAGE={}", runtime_image(offline)?));
        }
        checked(build.arg(&context))?;
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
    admission.require_for(mode)?;
    let bundle = c.bundle()?;
    let report = serde_json::json!({ "image": image, "admission": admission, "bundle": bundle,
        "language": c.build.language, "next": "harmony check, then harmony search" });
    Ok(report)
}

fn declares_runtime_image(dockerfile: &str) -> bool {
    dockerfile.lines().any(|line| {
        let mut words = line.split_whitespace();
        words
            .next()
            .is_some_and(|word| word.eq_ignore_ascii_case("ARG"))
            && words
                .next()
                .is_some_and(|word| word.split('=').next() == Some("HARMONY_RUNTIME_IMAGE"))
    })
}

fn runtime_image(offline: bool) -> Result<String> {
    let script = sdk(offline)?.join("workloads/languages/build-runtime.sh");
    let output = Command::new("bash")
        .arg(&script)
        .env("HARMONY_CONTAINER_TOOL", container_tool()?)
        .stderr(std::process::Stdio::inherit())
        .output()?;
    let text = String::from_utf8(output.stdout)?;
    match text.lines().last() {
        Some(tag) if output.status.success() && !tag.is_empty() => Ok(tag.to_owned()),
        _ => Err(format!("{} failed; see its output above", script.display()).into()),
    }
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

pub fn init(path: &Path, language: Option<Language>, input: Option<String>) -> Result<()> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let dockerfile = match &input {
        Some(input) if is_dockerfile(Path::new(input)) => Some(PathBuf::from(input)),
        None if language.is_none() => ["Dockerfile", "Containerfile"]
            .into_iter()
            .map(|name| directory.join(name))
            .find(|path| path.is_file()),
        _ => None,
    };
    let mut legacy = Config::default();
    if let Some(dockerfile) = dockerfile {
        let dockerfile = fs::canonicalize(dockerfile)?;
        let context = build_context(&dockerfile);
        let base = fs::canonicalize(directory)?;
        legacy.image = Some(format!("harmony-{}:local", image_name(&dockerfile)));
        legacy.build.dockerfile = Some(relative(&dockerfile, &base));
        legacy.build.context = Some(relative(&context, &base));
    } else {
        let skeleton = language.is_some() || input.is_none();
        legacy.image = Some(input.unwrap_or_else(|| "harmony-app:local".into()));
        if let Some(language) = language {
            legacy.build.language = Some(language);
            legacy.build.context = Some(".".into());
        }
        if skeleton {
            let command = match language {
                Some(Language::Python) => vec!["/opt/python/bin/python3.14", "/app/main.py"],
                Some(Language::Java) => vec!["/opt/java/bin/java", "-jar", "/app/app.jar"],
                _ => vec!["/opt/harmony/application"],
            };
            legacy.nodes.insert(
                "app".into(),
                super::config::Node {
                    command: command.into_iter().map(str::to_owned).collect(),
                },
            );
        }
    }
    let mut shared = legacy.shared()?;
    prune(&mut shared.workload.options);
    let text = toml::to_string_pretty(&shared)?;
    file.write_all(text.as_bytes())?;
    Ok(())
}

fn prune(table: &mut toml::Table) {
    for (_, value) in table.iter_mut() {
        if let toml::Value::Table(inner) = value {
            prune(inner);
        }
    }
    table.retain(|_, value| match value {
        toml::Value::Array(values) => !values.is_empty(),
        toml::Value::Table(values) => !values.is_empty(),
        _ => true,
    });
}

fn is_dockerfile(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    path.is_file()
        && (["dockerfile", "containerfile"].contains(&lower.as_str())
            || lower.starts_with("dockerfile.")
            || lower.ends_with(".dockerfile")
            || lower.ends_with(".containerfile"))
}

fn build_context(dockerfile: &Path) -> PathBuf {
    let parent = dockerfile.parent().unwrap_or(Path::new("/"));
    parent
        .ancestors()
        .find(|directory| directory.join(".git").exists())
        .unwrap_or(parent)
        .to_path_buf()
}

fn image_name(dockerfile: &Path) -> String {
    let name = dockerfile
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or("app")
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    let name = name.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    if name.is_empty() {
        "app".into()
    } else {
        name.into()
    }
}

fn relative(path: &Path, base: &Path) -> PathBuf {
    match path.strip_prefix(base) {
        Ok(inner) if inner.as_os_str().is_empty() => PathBuf::from("."),
        Ok(inner) => inner.to_path_buf(),
        Err(_) => path.to_path_buf(),
    }
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

    #[test]
    fn a_dockerfile_input_builds_from_the_enclosing_repository_with_the_image_bundle() {
        let repository = tempfile::tempdir().unwrap();
        fs::create_dir(repository.path().join(".git")).unwrap();
        let target = repository.path().join("targets/kv store");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("Dockerfile"), "FROM scratch\n").unwrap();
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("harmony.toml");
        init(
            &path,
            None,
            Some(target.join("Dockerfile").display().to_string()),
        )
        .unwrap();
        let mut shared: crate::config::Config =
            toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        shared.workload.package = "faults".into();
        let config = Config::from_shared(&shared).unwrap();
        assert_eq!(config.image.as_deref(), Some("harmony-kv-store:local"));
        assert_eq!(
            config.build.dockerfile,
            Some(fs::canonicalize(target.join("Dockerfile")).unwrap())
        );
        assert_eq!(
            config.build.context,
            Some(fs::canonicalize(repository.path()).unwrap())
        );
        assert!(config.nodes.is_empty());
        assert!(config.build.language.is_none());
    }

    #[test]
    fn only_harmony_runtime_arguments_select_the_runtime_image() {
        assert!(declares_runtime_image(
            "ARG HARMONY_RUNTIME_IMAGE=harmony-language-runtime:local\nFROM x\n"
        ));
        assert!(declares_runtime_image("arg HARMONY_RUNTIME_IMAGE\n"));
        assert!(!declares_runtime_image(
            "ARG HARMONY_RUNTIME_IMAGES=x\n# ARG HARMONY_RUNTIME_IMAGE\n"
        ));
    }
}

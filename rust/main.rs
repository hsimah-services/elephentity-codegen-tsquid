//! `eleph-gen-tsquid`: compiles tsquid's route manifest for the app's server, in PHP or C#.
//!
//! The validation is language-neutral (`validate.rs`); only the output differs (`emit_php.rs`, `emit_csharp.rs`). The target's
//! `language` is `"php"` (the default, and unchanged) or `"csharp"`, which also needs a `namespace`.

mod emit_csharp;
mod emit_php;
mod validate;

use serde_json::{json, Value};
use std::{
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

/// C# reserved words, which cannot be a namespace segment without an `@` escape.
const KEYWORDS: &[&str] = &[
    "abstract",
    "as",
    "base",
    "bool",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "checked",
    "class",
    "const",
    "continue",
    "decimal",
    "default",
    "delegate",
    "do",
    "double",
    "else",
    "enum",
    "event",
    "explicit",
    "extern",
    "false",
    "finally",
    "fixed",
    "float",
    "for",
    "foreach",
    "goto",
    "if",
    "implicit",
    "in",
    "int",
    "interface",
    "internal",
    "is",
    "lock",
    "long",
    "namespace",
    "new",
    "null",
    "object",
    "operator",
    "out",
    "override",
    "params",
    "private",
    "protected",
    "public",
    "readonly",
    "ref",
    "return",
    "sbyte",
    "sealed",
    "short",
    "sizeof",
    "stackalloc",
    "static",
    "string",
    "struct",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "uint",
    "ulong",
    "unchecked",
    "unsafe",
    "ushort",
    "using",
    "virtual",
    "void",
    "volatile",
    "while",
];

/// Whether `namespace` is a dotted run of identifiers, none of them a reserved word.
fn is_namespace(namespace: &str) -> bool {
    !namespace.is_empty()
        && namespace.split('.').all(|segment| {
            let mut characters = segment.chars();

            characters
                .next()
                .is_some_and(|first| first == '_' || first.is_alphabetic())
                && characters.all(|rest| rest == '_' || rest.is_alphanumeric())
                && !KEYWORDS.contains(&segment)
        })
}

/// Which language a run writes. PHP is what every existing project gets.
#[derive(PartialEq)]
enum Language {
    Php,
    Csharp,
}

fn language(config: &Value) -> Result<Language, String> {
    match config.get("language") {
        None | Some(Value::Null) => Ok(Language::Php),
        Some(Value::String(name)) if name == "php" => Ok(Language::Php),
        Some(Value::String(name)) if name == "csharp" => Ok(Language::Csharp),
        _ => Err("The tsquid target's \"language\" must be \"php\" or \"csharp\".".into()),
    }
}

fn response(files: Vec<Value>, errors: Vec<String>, language: &Language) -> Value {
    match language {
        Language::Php => {
            json!({"elephentity":1,"irVersion":"1.2","headerStyle":"php","extensions":["php"],"files":files,"errors":errors})
        }
        // Nothing is claimed when generation failed, so a failed run sweeps nothing.
        Language::Csharp => {
            json!({"elephentity":1,"irVersion":"1.2","headerStyle":"line-comment","extensions": if errors.is_empty() { vec!["cs"] } else { vec![] },"files":files,"errors":errors})
        }
    }
}

fn run(request: &Value) -> Result<Value, String> {
    if request["elephentity"] != 1 || request["irVersion"] != "1.2" {
        return Err("Version mismatch: this builder speaks protocol 1 and IR 1.2.".into());
    }
    let kind = match request.get("request") {
        None | Some(Value::Null) => "generate",
        Some(Value::String(kind)) => kind,
        _ => return Err("Expected generate or describe request.".into()),
    };
    match kind {
        "describe" => {
            return Ok(
                json!({"elephentity":1,"irVersion":"1.2","provides":{"integrations":{"tsquid":{"description":"Preloads the initial tsquid route query into HTML.","projectConfig":{},"entityConfig":{},"queryConfig":{}}}}}),
            )
        }
        "generate" => (),
        _ => return Err("Expected generate or describe request.".into()),
    }
    let integrations = &request["schema"]["project"]["integrations"];
    if !integrations.is_object() && integrations != &json!([]) {
        return Err("Missing schema.project.integrations object.".into());
    }
    let language = match language(&request["config"]) {
        Ok(language) => language,
        Err(error) => return Ok(response(vec![], vec![error], &Language::Php)),
    };
    if integrations.get("tsquid").is_none() {
        return Ok(response(vec![], vec![], &language));
    }
    if language == Language::Csharp {
        let problems = namespace_problems(&request["config"]);
        if !problems.is_empty() {
            return Ok(response(vec![], problems, &language));
        }
    }
    let result = (|| {
        let manifest_path = request["config"]["manifest"].as_str().filter(|s| !s.is_empty()).ok_or("Set the tsquid target's manifest path (relative to its output directory, or absolute).")?;
        let output = request["outputDirectory"]
            .as_str()
            .ok_or("Missing outputDirectory.")?;
        // Resolve .. before opening: output directories may not exist on the first build.
        let mut path = PathBuf::new();
        for component in Path::new(output).join(manifest_path).components() {
            match component {
                Component::ParentDir if path.file_name().is_some_and(|name| name != "..") => {
                    path.pop();
                }
                Component::CurDir => (),
                other => path.push(other),
            }
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
        let manifest: Value =
            serde_json::from_str(&text).map_err(|e| format!("Invalid manifest JSON: {e}"))?;
        validate::validate(&manifest)?;
        Ok::<_, String>(match language {
            Language::Php => ("tsquid-manifest.php", emit_php::manifest(&manifest)),
            Language::Csharp => (
                "TsquidManifest.cs",
                emit_csharp::manifest(
                    &manifest,
                    request["config"]["namespace"].as_str().unwrap_or(""),
                ),
            ),
        })
    })();
    Ok(match result {
        Ok((path, body)) => response(vec![json!({"path":path,"body":body})], vec![], &language),
        Err(error) => response(vec![], vec![error], &language),
    })
}

/// Every problem with a C# target's `namespace`.
fn namespace_problems(config: &Value) -> Vec<String> {
    match config.get("namespace") {
        Some(Value::String(namespace)) if is_namespace(namespace) => vec![],
        Some(Value::String(namespace)) if !namespace.is_empty() => vec![format!(
            "The tsquid target's \"namespace\" (\"{namespace}\") is not a valid C# namespace: use dot-separated identifiers that are not reserved words."
        )],
        _ => vec!["The tsquid target needs \"namespace\" set to a non-empty string when \"language\" is \"csharp\".".into()],
    }
}

fn main() {
    let mut input = String::new();
    let result = io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| e.to_string())
        .and_then(|_| serde_json::from_str(&input).map_err(|e| e.to_string()))
        .and_then(|v| run(&v));
    match result {
        Ok(v) => println!("{v}"),
        Err(e) => {
            eprintln!("eleph-gen-tsquid: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_and_generated_contract() {
        let vectors: Value =
            serde_json::from_str(include_str!("../tests/fixtures/vectors.json")).unwrap();
        validate::validate(&vectors["manifest"]).unwrap();
        let mut bad = vectors["manifest"].clone();
        bad["version"] = json!(2);
        assert!(validate::validate(&bad).is_err());
        assert!(run(&json!({"elephentity":1,"irVersion":"1.1","request":"describe"})).is_err());
        assert!(
            run(&json!({"elephentity":1,"irVersion":"1.2","request":"describe"})).unwrap()
                ["provides"]["integrations"]["tsquid"]
                .is_object()
        );
        assert_eq!(
            run(
                &json!({"elephentity":1,"irVersion":"1.2","schema":{"project":{"integrations":{}}}})
            )
            .unwrap()["files"],
            json!([])
        );
        assert_eq!(
            emit_php::php(&json!({"nested":{},"list":[],"quote":"'\\"})),
            "(object) ['list' => [], 'nested' => (object) [], 'quote' => '\\'\\\\']"
        );
    }

    #[test]
    fn namespaces_are_dotted_identifiers_that_are_not_keywords() {
        assert!(is_namespace("Hsimah.Entity"));
        assert!(!is_namespace("a..b"));
        assert!(!is_namespace("App.class"));
        assert!(!is_namespace(""));
    }
}

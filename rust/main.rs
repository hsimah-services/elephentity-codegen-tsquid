use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

#[derive(Deserialize)]
struct Manifest {
    version: u64,
    routes: Vec<Route>,
}
#[derive(Deserialize)]
struct Route {
    name: String,
    path: String,
    ssr: bool,
    fields: BTreeMap<String, Field>,
    query: Option<Query>,
}
#[derive(Deserialize)]
struct Field {
    kind: String,
    optional: bool,
    r#in: String,
}
#[derive(Deserialize)]
struct Query {
    operation: String,
    id: String,
    text: String,
    variables: BTreeMap<String, Binding>,
}
#[derive(Deserialize)]
struct Binding {
    input: Option<String>,
    #[serde(rename = "default")]
    _default: Option<Value>,
}

fn validate(value: &Value) -> Result<(), String> {
    let manifest: Manifest = serde_json::from_value(value.clone())
        .map_err(|e| format!("Invalid tsquid manifest: {e}"))?;
    if manifest.version != 1 {
        return Err("Unsupported tsquid manifest version; expected 1.".into());
    }
    let mut names = std::collections::BTreeSet::new();
    for route in manifest.routes {
        if route.name.is_empty()
            || !names.insert(route.name.clone())
            || !route.path.starts_with('/')
        {
            return Err("Route names must be unique and nonempty; paths must start with /.".into());
        }
        let _ = route.ssr;
        let params: Vec<_> = route
            .path
            .split('/')
            .filter_map(|s| s.strip_prefix(':'))
            .collect();
        for (name, field) in &route.fields {
            if !["string", "int", "bool", "string[]", "int[]", "bool[]"]
                .contains(&field.kind.as_str())
                || !["path", "query"].contains(&field.r#in.as_str())
                || (field.r#in == "path"
                    && (field.optional
                        || field.kind.ends_with("[]")
                        || !params.contains(&name.as_str())))
            {
                return Err(format!("Invalid field {}.{name}.", route.name));
            }
        }
        if params
            .iter()
            .any(|p| route.fields.get(*p).is_none_or(|f| f.r#in != "path"))
        {
            return Err(format!("Missing path field on {}.", route.name));
        }
        if let Some(query) = route.query {
            if query.operation.is_empty() || query.text.trim().is_empty() || query.id.is_empty() {
                return Err(format!("Incomplete query on {}.", route.name));
            }
            for binding in query.variables.values() {
                if binding
                    .input
                    .as_ref()
                    .is_some_and(|name| !route.fields.contains_key(name))
                {
                    return Err(format!("Unknown query input on {}.", route.name));
                }
            }
        }
    }
    Ok(())
}

fn php(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(v) => v.to_string(),
        Value::Number(v) => v.to_string(),
        Value::String(v) => format!("'{}'", v.replace('\\', "\\\\").replace('\'', "\\'")),
        Value::Array(v) => format!("[{}]", v.iter().map(php).collect::<Vec<_>>().join(", ")),
        Value::Object(v) => format!(
            "(object) [{}]",
            v.iter()
                .map(|(k, v)| format!("{} => {}", php(&json!(k)), php(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn response(files: Vec<Value>, errors: Vec<String>) -> Value {
    json!({"elephentity":1,"irVersion":"1.2","headerStyle":"php","extensions":["php"],"files":files,"errors":errors})
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
    if integrations.get("tsquid").is_none() {
        return Ok(response(vec![], vec![]));
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
        validate(&manifest)?;
        Ok::<_, String>(format!(
            "return new \\Eleph\\Tsquid\\Manifest({});\n",
            php(&manifest)
        ))
    })();
    Ok(match result {
        Ok(body) => response(
            vec![json!({"path":"tsquid-manifest.php","body":body})],
            vec![],
        ),
        Err(error) => response(vec![], vec![error]),
    })
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
        validate(&vectors["manifest"]).unwrap();
        let mut bad = vectors["manifest"].clone();
        bad["version"] = json!(2);
        assert!(validate(&bad).is_err());
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
            php(&json!({"nested":{},"list":[],"quote":"'\\"})),
            "(object) ['list' => [], 'nested' => (object) [], 'quote' => '\\'\\\\']"
        );
    }
}

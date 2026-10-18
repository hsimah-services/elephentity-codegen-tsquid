//! The language-neutral half: what a tsquid `routes.manifest.json` must be before any language is written from it.

use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

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

pub fn validate(value: &Value) -> Result<(), String> {
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

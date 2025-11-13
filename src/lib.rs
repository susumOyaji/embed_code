use regex::Regex;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use worker::*;

/// Recursively finds paths to objects that contain all the specified keys.
fn find_object_paths<'a>(
    value: &'a Value,
    keys_to_find: &[String],
    current_path: &mut Vec<&'a str>,
    found_paths: &mut Vec<Vec<&'a str>>,
) {
    if let Value::Object(map) = value {
        let has_all_keys = keys_to_find.iter().all(|key| map.contains_key(key));
        if has_all_keys {
            found_paths.push(current_path.clone());
        }

        for (key, nested_value) in map.iter() {
            current_path.push(key);
            find_object_paths(nested_value, keys_to_find, current_path, found_paths);
            current_path.pop();
        }
    } else if let Value::Array(arr) = value {
        for nested_value in arr {
            find_object_paths(nested_value, keys_to_find, current_path, found_paths);
        }
    }
}

fn to_pascal_case(s: &str) -> String {
    s.split(|c: char| c == '_' || !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|part| {
            let mut c = part.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            }
        })
        .collect::<String>()
}

/// Recursively generates struct definitions and returns them as a Vec of strings.
fn generate_structs(
    name: &str,
    value: &Value,
    all_defs: &mut BTreeMap<String, String>,
) -> Vec<String> {
    if let Value::Object(map) = value {
        let struct_name = to_pascal_case(name);
        if all_defs.contains_key(&struct_name) {
            return Vec::new();
        }

        let mut fields = Vec::new();
        let mut nested_defs_to_print = Vec::new();

        for (k, v) in map {
            let field_name = k;
            let field_type = match v {
                Value::Object(_) => {
                    let nested_name = to_pascal_case(field_name);
                    nested_defs_to_print.extend(generate_structs(&nested_name, v, all_defs));
                    format!("Option<{}>", nested_name)
                }
                Value::Array(arr) => {
                    if let Some(first) = arr.first() {
                        match first {
                            Value::Object(_) => {
                                let singular_name = field_name.strip_suffix('s').unwrap_or(field_name);
                                let nested_name = to_pascal_case(singular_name);
                                nested_defs_to_print.extend(generate_structs(&nested_name, first, all_defs));
                                format!("Option<Vec<{}>>", nested_name)
                            }
                            _ => "Option<Vec<serde_json::Value>>".to_string(),
                        }
                    } else {
                        "Option<Vec<serde_json::Value>>".to_string()
                    }
                }
                Value::String(_) => "Option<String>".to_string(),
                Value::Number(n) => {
                    if n.is_i64() { "Option<i64>".to_string() }
                    else if n.is_u64() { "Option<u64>".to_string() }
                    else { "Option<f64>".to_string() }
                }
                Value::Bool(_) => "Option<bool>".to_string(),
                Value::Null => "Option<serde_json::Value>".to_string(),
            };
            fields.push(format!("    pub {}: {},", field_name, field_type));
        }

        let struct_def = format!(
            "#[derive(Debug, serde::Deserialize)]\n\
             pub struct {} {{\n{}\n}}",
            struct_name,
            fields.join("\n")
        );

        all_defs.insert(struct_name.clone(), struct_def.clone());

        let mut result = vec![struct_def];
        result.extend(nested_defs_to_print);
        result
    } else {
        Vec::new()
    }
}

#[event(fetch)]
pub async fn main(req: Request, _env: Env, _ctx: worker::Context) -> Result<Response> {
    let url = req.url()?;
    let query_pairs = url.query_pairs();

    let mut target_url = None;
    let mut keys = Vec::new();
    let mut display_keys = Vec::new();

    for (k, v) in query_pairs {
        match k.as_ref() {
            "url" => target_url = Some(v.to_string()),
            "key" => keys.push(v.to_string()),
            "display" => display_keys.push(v.to_string()),
            _ => {}
        }
    }

    let target_url = match target_url {
        Some(u) => u,
        None => return Response::error("Missing ?url= parameter", 400),
    };

    let mut resp = Fetch::Url(target_url.parse().unwrap()).send().await?;
    let body = resp.text().await?;

    let re = Regex::new(r"(?s)window\.__PRELOADED_STATE__\s*=\s*(.*?)</script>")
        .map_err(|e| Error::RustError(e.to_string()))?;

    let mut output = String::new();

    if let Some(caps) = re.captures(&body) {
        if let Some(json_match) = caps.get(1) {
            let mut json_str = json_match.as_str().trim();
            if json_str.ends_with(';') {
                json_str = &json_str[..json_str.len() - 1];
            }

            let data: Value = serde_json::from_str(json_str)
                .map_err(|e| Error::RustError(e.to_string()))?;

            let mut found_paths = Vec::new();
            find_object_paths(&data, &keys, &mut Vec::new(), &mut found_paths);

            if found_paths.is_empty() {
                output.push_str(&format!(
                    "No objects found with the specified keys: {:?}\n",
                    keys
                ));
            } else {
                let mut all_defs = BTreeMap::new();
                let mut name_counts = HashMap::new();

                for path in found_paths.iter() {
                    let mut target_obj = &data;
                    for &key in path.iter() {
                        target_obj = &target_obj[key];
                    }

                    let object_key = *path.last().unwrap_or(&"Root");
                    let base_name = to_pascal_case(object_key);

                    let count = name_counts.entry(base_name.clone()).or_insert(0);
                    *count += 1;

                    let struct_name = if *count > 1 {
                        format!("{}{}", base_name, count)
                    } else {
                        base_name
                    };

                    output.push_str(&format!("Path: {:?}\n", path));

                    if let Value::Object(map) = target_obj {
                        output.push_str("// --- Extracted Values ---\n");
                        if display_keys.is_empty() {
                            for (key, value) in map {
                                output.push_str(&format!("{} = {:?}\n", key, value));
                            }
                        } else {
                            for key in &display_keys {
                                if let Some(value) = map.get(key) {
                                    output.push_str(&format!("{} = {:?}\n", key, value));
                                }
                            }
                        }
                    }

                    output.push_str("// --- Struct Definition ---\n");
                    for def in generate_structs(&struct_name, target_obj, &mut all_defs) {
                        output.push_str(&format!("{}\n\n", def));
                    }
                }
            }
        }
    } else {
        output.push_str("Could not find window.__PRELOADED_STATE__ script tag.");
    }

    Response::ok(output)
}

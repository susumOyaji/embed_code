// Fixed and improved version of the Worker code
// - Safety fixes for find_object_paths (no lifetime issues)
// - More robust JSON traversal and string extraction
// - Safer URL parsing and fetch error handling
// - Minor DOM selector fallbacks

use futures::future::join_all;
use regex::Regex;
use scraper::{Html, Selector};
use serde::{Serialize};
use serde_json::Value;
use worker::*;

fn set_panic_hook() {
    console_error_panic_hook::set_once();
}

struct DataSource {
    path: &'static [&'static str],
    code_key: &'static str,
    name_key: &'static str,
    price_key: &'static str,
    change_key: &'static str,
    change_rate_key: &'static str,
    time_key: &'static str,
    strip_suffix: bool,
}

#[derive(Serialize, Debug, Clone)]
struct NormalizedData {
    code: String,
    name: String,
    price: String,
    price_change: String,
    price_change_rate: String,
    update_time: String,
    status: String,
    source: String,
}

#[derive(Serialize, Debug)]
struct CodeResult {
    code: String,
    data: Option<NormalizedData>,
    error: Option<String>,
}

#[event(fetch)]
pub async fn main(req: Request, _env: Env, _ctx: Context) -> Result<Response> {
    set_panic_hook();

    let url = match req.url() {
        Ok(u) => u,
        Err(e) => return Response::error(format!("Invalid request URL: {}", e), 400),
    };

    let codes_query = url.query_pairs().find(|(key, _)| key == "code");

    if codes_query.is_none() {
        return Response::error("Query parameter 'code' is required. e.g., ?code=7203.T,^DJI", 400);
    }

    let codes: Vec<String> = codes_query
        .unwrap()
        .1
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.trim().to_string())
        .collect();

    if codes.is_empty() {
        return Response::error("Query parameter 'code' cannot be empty.", 400);
    }

    let futures = codes.iter().map(|code| fetch_single_code(code.clone()));
    let results = join_all(futures).await;

    Response::from_json(&results)
}

async fn fetch_single_code(code: String) -> CodeResult {
    let url_str = if code.starts_with('^') || code.contains('=') || code.ends_with(".T") || code.ends_with(".O") {
        format!("https://finance.yahoo.co.jp/quote/{}/", code)
    } else {
        format!("https://finance.yahoo.co.jp/quote/{}.T/", code)
    };

    let parsed = match url_str.parse() {
        Ok(u) => u,
        Err(e) => return CodeResult { code, data: None, error: Some(format!("Invalid URL: {}", e)) },
    };

    let body = match Fetch::Url(parsed).send().await {
        Ok(mut resp) => match resp.text().await {
            Ok(text) => text,
            Err(e) => return CodeResult { code, data: None, error: Some(format!("Failed to read response text: {}", e)) },
        },
        Err(e) => return CodeResult { code, data: None, error: Some(format!("Failed to fetch URL: {}", e)) },
    };

    // Capture the JSON assigned to window.__PRELOADED_STATE__
    let re = Regex::new(r"(?s)window\.__PRELOADED_STATE__\s*=\s*(\{.*?\})\s*</script>").unwrap();

    let result_data = if let Some(caps) = re.captures(&body) {
        if let Some(json_match) = caps.get(1) {
            let json_str = json_match.as_str().trim();
            match serde_json::from_str::<Value>(json_str) {
                Ok(data) => process_json_data(&code, &data),
                Err(e) => Err(worker::Error::from(format!("Failed to parse JSON: {}", e))),
            }
        } else {
            process_dom_data(&code, &body)
        }
    } else {
        process_dom_data(&code, &body)
    };

    match result_data {
        Ok(data) => CodeResult { code, data: Some(data), error: None },
        Err(e) => CodeResult { code, data: None, error: Some(e.to_string()) },
    }
}

fn process_json_data(code: &str, data: &Value) -> Result<NormalizedData> {
    let data_sources = get_data_sources();

    for source in &data_sources {
        if let Some(target_obj) = find_object(data, source.path) {
            if let Some(found_code) = get_string_value(target_obj, source.code_key) {
                let code_to_compare = if source.strip_suffix { code.split('.').next().unwrap_or(code) } else { code };
                if found_code.trim() == code_to_compare {
                    return Ok(NormalizedData {
                        code: code.to_string(),
                        name: get_string_value(target_obj, source.name_key).unwrap_or("N/A").to_string(),
                        price: extract_value_as_string(target_obj.get(source.price_key)),
                        price_change: extract_value_as_string(target_obj.get(source.change_key)),
                        price_change_rate: extract_value_as_string(target_obj.get(source.change_rate_key)),
                        update_time: get_string_value(target_obj, source.time_key).unwrap_or("N/A").to_string(),
                        status: "OK".to_string(),
                        source: "json_predefined".to_string(),
                    });
                }
            }
        }
    }

    // Generic fallback search
    let fallback_keys = vec!["code".to_string(), "name".to_string()];
    let mut found_paths: Vec<Vec<String>> = Vec::new();
    find_object_paths(data, &fallback_keys, &mut Vec::new(), &mut found_paths);

    for path in found_paths {
        let mut target = data;
        let mut ok = true;
        for key in &path {
            if let Some(next) = target.get(key) {
                target = next;
            } else {
                ok = false;
                break;
            }
        }
        if !ok { continue; }
        if let Some(found_code) = get_string_value(target, "code") {
            let code_to_compare = code.split('.').next().unwrap_or(code);
            if found_code.trim() == code_to_compare {
                return Ok(NormalizedData {
                    code: code.to_string(),
                    name: get_string_value(target, "name").unwrap_or("N/A").to_string(),
                    price: extract_value_as_string(target.get("price")),
                    price_change: extract_value_as_string(target.get("priceChange")),
                    price_change_rate: extract_value_as_string(target.get("priceChangeRate")),
                    update_time: extract_value_as_string(target.get("priceDateTime")),
                    status: "OK".to_string(),
                    source: "json_fallback".to_string(),
                });
            }
        }
    }

    Err(worker::Error::from("Could not find matching data in JSON."))
}

fn process_dom_data(code: &str, body: &str) -> Result<NormalizedData> {
    let document = Html::parse_document(body);

    let name_selector = Selector::parse("h1").unwrap();
    let price_selector = Selector::parse("[data-test='price']").ok();
    let price_selector2 = Selector::parse("div[class*='_CommonPriceBoard__priceBlock'] span").ok();
    let change_selector = Selector::parse("[data-test='price-change']").ok();
    let time_selector = Selector::parse("time").ok();

    let name = document.select(&name_selector).next().map(|el| el.text().collect::<String>().trim().to_string());

    let price = price_selector
        .and_then(|sel| document.select(&sel).next().map(|el| el.text().collect::<String>().trim().to_string()))
        .or_else(|| {
            price_selector2.and_then(|sel| document.select(&sel).next().map(|el| el.text().collect::<String>().trim().to_string()))
        });

    if name.is_none() || price.is_none() {
        return Err(worker::Error::from("Failed to scrape essential data (name/price) from DOM."));
    }

    let price_change = change_selector
        .and_then(|sel| document.select(&sel).next().map(|el| el.text().collect::<String>().trim().to_string()))
        .unwrap_or_else(|| "N/A".to_string());

    let update_time = time_selector
        .and_then(|sel| document.select(&sel).next().map(|el| el.text().collect::<String>().trim().to_string()))
        .unwrap_or_else(|| "N/A".to_string());

    Ok(NormalizedData {
        code: code.to_string(),
        name: name.unwrap(),
        price: price.unwrap(),
        price_change,
        price_change_rate: "N/A".to_string(),
        update_time,
        status: "OK".to_string(),
        source: "dom_fallback".to_string(),
    })
}

fn get_data_sources() -> Vec<DataSource> {
    vec![
        DataSource {
            path: &["mainStocksPriceBoard", "priceBoard"],
            code_key: "code",
            name_key: "name",
            price_key: "price",
            change_key: "priceChange",
            change_rate_key: "priceChangeRate",
            time_key: "priceDateTime",
            strip_suffix: true,
        },
        DataSource {
            path: &["mainCurrencyPriceBoard", "currencyPrices"],
            code_key: "currencyPairCode",
            name_key: "currencyPairName",
            price_key: "bid",
            change_key: "priceChange",
            change_rate_key: "priceChangeRate",
            time_key: "priceUpdateTime",
            strip_suffix: false,
        },
        DataSource {
            path: &["mainDomesticIndexPriceBoard", "indexPrices"],
            code_key: "code",
            name_key: "name",
            price_key: "price",
            change_key: "changePrice",
            change_rate_key: "changePriceRate",
            time_key: "japanUpdateTime",
            strip_suffix: false,
        },
    ]
}

fn find_object<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

fn get_string_value<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

fn extract_value_as_string(opt: Option<&Value>) -> String {
    match opt {
        Some(v) => {
            if let Some(s) = v.as_str() {
                s.to_string()
            } else if let Some(n) = v.as_f64() {
                // format numbers without trailing .0 when not needed
                if n.fract() == 0.0 {
                    format!("{:.0}", n)
                } else {
                    n.to_string()
                }
            } else {
                v.to_string().trim_matches('"').to_string()
            }
        }
        None => "N/A".to_string(),
    }
}

fn find_object_paths(
    value: &Value,
    keys_to_find: &[String],
    current_path: &mut Vec<String>,
    found_paths: &mut Vec<Vec<String>>,
) {
    match value {
        Value::Object(map) => {
            if keys_to_find.iter().all(|k| map.contains_key(k)) {
                found_paths.push(current_path.clone());
            }
            for (key, nested_value) in map {
                current_path.push(key.clone());
                find_object_paths(nested_value, keys_to_find, current_path, found_paths);
                current_path.pop();
            }
        }
        Value::Array(arr) => {
            for nested_value in arr {
                find_object_paths(nested_value, keys_to_find, current_path, found_paths);
            }
        }
        _ => {}
    }
}

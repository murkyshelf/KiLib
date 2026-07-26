//! LCSC / EasyEDA component API.
//!
//! Two public endpoints are used, both anonymous:
//!
//! * `easyeda.com/api/eda/product/search` — keyword search over the LCSC catalogue
//! * `easyeda.com/api/products/<LCSC>/components` — symbol + footprint geometry
//! * `modules.easyeda.com/qAxj…/<uuid>` — the STEP model referenced by the footprint
//!
//! The geometry is stored as `~`-delimited records; [`Record`] is the shared
//! reader for them.

use serde_json::Value;

use crate::http;
use crate::logging;

const SEARCH_URL: &str = "https://easyeda.com/api/eda/product/search";
const COMPONENT_URL: &str = "https://easyeda.com/api/products";
const STEP_URL: &str = "https://modules.easyeda.com/qAxj6KHrDKw4blvCG8QJPs7Y";
const SEARCH_VERSION: &str = "6.5.31";
const COMPONENT_VERSION: &str = "6.4.19.5";

/// One EasyEDA shape record, e.g. `PAD~RECT~3973.133~…`.
///
/// Records are `~`-delimited, and pins additionally split into `^^` groups.
/// Missing trailing fields are normal, so every accessor tolerates a short
/// record rather than failing the whole import.
pub struct Record<'a> {
    fields: Vec<&'a str>,
}

impl<'a> Record<'a> {
    pub fn new(text: &'a str) -> Self {
        Record {
            fields: text.split('~').collect(),
        }
    }

    pub fn kind(&self) -> &'a str {
        self.fields.first().copied().unwrap_or("")
    }

    pub fn str(&self, index: usize) -> &'a str {
        self.fields.get(index).copied().unwrap_or("")
    }

    pub fn num(&self, index: usize) -> f64 {
        self.str(index).trim().parse().unwrap_or(0.0)
    }

    /// A field that must be present and numeric for the shape to mean anything.
    pub fn opt_num(&self, index: usize) -> Option<f64> {
        self.str(index).trim().parse().ok()
    }
}

/// Splits a pin record into its `^^` groups.
pub fn groups(text: &str) -> Vec<&str> {
    text.split("^^").collect()
}

// ------------------------------------------------------------------ searching

#[derive(Clone, Debug, Default)]
pub struct Hit {
    /// LCSC part number, e.g. `C7420`. This is what [`fetch`] takes.
    pub id: String,
    pub mpn: String,
    pub manufacturer: String,
    pub package: String,
    pub category: String,
    pub stock: u64,
}

pub fn search(query: &str, limit: usize) -> Result<Vec<Hit>, String> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let url = format!(
        "{SEARCH_URL}?keyword={}&version={SEARCH_VERSION}&page=1&limit={limit}",
        http::encode(query)
    );
    let body = http::get_string(&url)?;
    let json: Value =
        serde_json::from_str(&body).map_err(|e| format!("search response was not JSON: {e}"))?;

    let list = json["result"]["productList"]
        .as_array()
        .ok_or_else(|| "search response had no productList".to_string())?;

    let hits: Vec<Hit> = list
        .iter()
        .filter_map(|item| {
            let id = item["number"].as_str()?.to_string();
            Some(Hit {
                id,
                mpn: item["mpn"].as_str().unwrap_or_default().to_string(),
                manufacturer: item["manufacturer"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                package: item["package"].as_str().unwrap_or_default().to_string(),
                category: category_from_url(item["url"].as_str().unwrap_or_default()),
                stock: item["stock"].as_u64().unwrap_or(0),
            })
        })
        .collect();

    logging::info(format!("search \"{query}\" -> {} result(s)", hits.len()));
    Ok(hits)
}

/// The catalogue category is only present inside the product URL, as in
/// `/product-detail/555-Timers---Counters_YONGYUTAI-NE555_C52195098.html`.
fn category_from_url(url: &str) -> String {
    url.rsplit('/')
        .next()
        .and_then(|tail| tail.split('_').next())
        .map(|slug| slug.replace("---", " / ").replace('-', " "))
        .unwrap_or_default()
}

// ------------------------------------------------------------------ fetching

/// Everything the converters need about one component.
pub struct Component {
    pub lcsc: String,
    pub title: String,
    pub manufacturer: String,
    pub datasheet: String,
    pub prefix: String,
    /// Schematic origin, in EasyEDA units.
    pub symbol_origin: (f64, f64),
    pub symbol_shapes: Vec<String>,
    pub package_name: String,
    /// Footprint origin, in EasyEDA units.
    pub package_origin: (f64, f64),
    pub package_shapes: Vec<String>,
}

pub fn component(lcsc: &str) -> Result<Component, String> {
    let lcsc = lcsc.trim().to_uppercase();
    if lcsc.is_empty() {
        return Err("no LCSC part number given".to_string());
    }
    let url = format!("{COMPONENT_URL}/{lcsc}/components?version={COMPONENT_VERSION}");
    let body = http::get_string(&url)?;
    let json: Value =
        serde_json::from_str(&body).map_err(|e| format!("component response was not JSON: {e}"))?;

    if json["success"].as_bool() == Some(false) {
        let message = json["message"].as_str().unwrap_or("component not found");
        return Err(format!("{lcsc}: {message}"));
    }
    let result = &json["result"];
    if result.is_null() {
        return Err(format!("{lcsc}: no such component on LCSC"));
    }

    let data = &result["dataStr"];
    let para = &data["head"]["c_para"];
    let package = &result["packageDetail"]["dataStr"];

    let title = result["title"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| para["name"].as_str())
        .unwrap_or(&lcsc)
        .to_string();

    Ok(Component {
        lcsc: lcsc.clone(),
        title,
        manufacturer: para["Manufacturer"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        datasheet: result["lcsc"]["url"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        prefix: para["pre"].as_str().unwrap_or("U?").to_string(),
        symbol_origin: (number(&data["head"]["x"]), number(&data["head"]["y"])),
        symbol_shapes: strings(&data["shape"]),
        package_name: result["packageDetail"]["title"]
            .as_str()
            .or_else(|| para["package"].as_str())
            .unwrap_or_default()
            .to_string(),
        package_origin: (number(&package["head"]["x"]), number(&package["head"]["y"])),
        package_shapes: strings(&package["shape"]),
    })
}

/// Downloads the STEP model a footprint's `SVGNODE` record points at.
pub fn step_model(uuid: &str) -> Result<Vec<u8>, String> {
    let body = http::get_bytes(&format!("{STEP_URL}/{uuid}"))?;
    // The CDN answers a missing key with an XML error document and HTTP 200.
    if body.starts_with(b"<?xml") || !body.starts_with(b"ISO-10303-21") {
        return Err(format!("no STEP model published for {uuid}"));
    }
    Ok(body)
}

/// EasyEDA writes numbers as either JSON numbers or strings depending on the
/// field and the editor version that saved the part.
fn number(value: &Value) -> f64 {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0.0)
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

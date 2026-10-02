use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

#[derive(Debug)]
pub enum ParseError {
    TooLarge(usize),
    InvalidJson(serde_json::Error),
    NotAnObject,
    MissingPaths,
    RefCycle(String),
    RefDepth(String),
    NoOperations,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge(n) => write!(f, "specification is {n} bytes, limit is 16MB"),
            Self::InvalidJson(e) => write!(f, "invalid JSON: {e}"),
            Self::NotAnObject => write!(f, "specification must be a JSON object"),
            Self::MissingPaths => write!(f, "specification has no paths"),
            Self::RefCycle(r) => write!(f, "circular $ref: {r}"),
            Self::RefDepth(r) => write!(f, "$ref too deep: {r}"),
            Self::NoOperations => write!(f, "specification contains no operations"),
        }
    }
}

impl std::error::Error for ParseError {}

pub struct Api {
    pub operations: Vec<Operation>,
    pub common_auth: Option<AuthInfo>,
}

pub struct Operation {
    pub name: String,
    pub summary: String,
    pub method: String,
    pub path: String,
    pub tags: Vec<String>,
    pub parameters: Vec<Parameter>,
    pub request_body: Option<RequestBody>,
    pub responses: BTreeMap<String, Response>,
}

#[derive(Clone)]
pub struct Parameter {
    pub name: String,
    pub location: String, // query, path, header, cookie
    pub required: bool,
    pub description: String,
    pub schema: SchemaInfo,
}

pub struct RequestBody {
    pub required: bool,
    pub fields: Vec<BodyField>,
}

pub struct BodyField {
    pub name: String,
    pub required: bool,
    pub description: String,
    pub schema: SchemaInfo,
}

pub struct Response {
    pub description: String,
    pub fields: Vec<BodyField>,
}

#[derive(Clone)]
pub struct SchemaInfo {
    pub type_name: String,
    pub format: Option<String>,
    pub enum_values: Option<Vec<String>>,
    pub description: Option<String>,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub example: Option<Value>,
    pub items_type: Option<String>,
}

#[derive(Clone)]
pub struct AuthInfo {
    pub header_name: String,
    pub description: String,
}

const MAX_REF_DEPTH: usize = 20;

pub fn parse(json_bytes: &[u8]) -> Result<Api, ParseError> {
    let root: Value = serde_json::from_slice(json_bytes).map_err(ParseError::InvalidJson)?;
    let obj = root.as_object().ok_or(ParseError::NotAnObject)?;
    let paths = obj
        .get("paths")
        .and_then(|v| v.as_object())
        .ok_or(ParseError::MissingPaths)?;

    let mut operations = Vec::new();
    let mut auth_params_seen: HashMap<String, usize> = HashMap::new();
    let mut total_ops = 0u32;

    let methods = ["get", "post", "put", "patch", "delete", "head", "options"];

    for (path, path_item) in paths {
        let path_obj = match path_item.as_object() {
            Some(o) => o,
            None => continue,
        };
        let path_params =
            extract_parameters(path_obj.get("parameters"), &root, &mut HashSet::new());

        for method in &methods {
            let op_val = match path_obj.get(*method) {
                Some(v) => v,
                None => continue,
            };
            total_ops += 1;

            let op_obj = match op_val.as_object() {
                Some(o) => o,
                None => continue,
            };

            let raw_id = op_obj
                .get("operationId")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let name = clean_operation_name(raw_id, method, path);

            let summary = op_obj
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let tags: Vec<String> = op_obj
                .get("tags")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            let mut params = path_params.clone();
            params.extend(extract_parameters(
                op_obj.get("parameters"),
                &root,
                &mut HashSet::new(),
            ));

            for p in &params {
                if p.location == "header" {
                    *auth_params_seen.entry(p.name.clone()).or_insert(0) += 1;
                }
            }

            let request_body =
                extract_request_body(op_obj.get("requestBody"), &root, &mut HashSet::new());

            let responses = extract_responses(op_obj.get("responses"), &root, &mut HashSet::new());

            operations.push(Operation {
                name,
                summary,
                method: method.to_uppercase(),
                path: path.clone(),
                tags,
                parameters: params,
                request_body,
                responses,
            });
        }
    }

    if operations.is_empty() {
        return Err(ParseError::NoOperations);
    }

    let common_auth = detect_common_auth(&auth_params_seen, total_ops, &operations);

    if let Some(ref auth) = common_auth {
        for op in &mut operations {
            op.parameters
                .retain(|p| !(p.location == "header" && p.name == auth.header_name));
        }
    }

    operations.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(Api {
        operations,
        common_auth,
    })
}

fn clean_operation_name(operation_id: &str, method: &str, path: &str) -> String {
    if !operation_id.is_empty() {
        // "SaleInvoiceController_getInvoice" -> "get_invoice"
        let name = if let Some((_controller, action)) = operation_id.split_once('_') {
            action.to_string()
        } else {
            operation_id.to_string()
        };
        to_snake_case(&name)
    } else {
        let clean_path = path
            .trim_matches('/')
            .replace('/', "_")
            .replace(['{', '}'], "");
        format!("{}_{}", method, to_snake_case(&clean_path))
    }
}

fn to_snake_case(s: &str) -> String {
    let mut result = String::with_capacity(s.len() + 4);
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() {
            let prev_lower = i > 0 && chars[i - 1].is_lowercase();
            let next_lower = i + 1 < chars.len() && chars[i + 1].is_lowercase();
            if prev_lower || (i > 0 && next_lower && chars[i - 1].is_uppercase()) {
                result.push('_');
            }
            result.push(c.to_lowercase().next().unwrap());
        } else if c == '-' || c == ' ' {
            result.push('_');
        } else {
            result.push(c);
        }
    }
    // collapse repeated underscores
    let mut out = String::with_capacity(result.len());
    let mut prev_underscore = false;
    for c in result.chars() {
        if c == '_' {
            if !prev_underscore {
                out.push(c);
            }
            prev_underscore = true;
        } else {
            out.push(c);
            prev_underscore = false;
        }
    }
    out.trim_matches('_').to_string()
}

fn resolve_ref<'a>(
    val: &'a Value,
    root: &'a Value,
    _seen: &mut HashSet<String>,
) -> Result<&'a Value, ParseError> {
    let mut current = val;
    let mut local_seen = HashSet::new();
    let mut depth = 0;
    loop {
        match current.get("$ref").and_then(|v| v.as_str()) {
            Some(ref_str) => {
                if !local_seen.insert(ref_str.to_string()) {
                    return Err(ParseError::RefCycle(ref_str.to_string()));
                }
                depth += 1;
                if depth > MAX_REF_DEPTH {
                    return Err(ParseError::RefDepth(ref_str.to_string()));
                }
                current = resolve_json_pointer(root, ref_str).unwrap_or(&Value::Null);
            }
            None => return Ok(current),
        }
    }
}

fn resolve_json_pointer<'a>(root: &'a Value, pointer: &str) -> Option<&'a Value> {
    let path = pointer.strip_prefix("#/")?;
    let mut current = root;
    for segment in path.split('/') {
        let decoded = segment.replace("~1", "/").replace("~0", "~");
        current = current.get(&decoded)?;
    }
    Some(current)
}

fn extract_parameters(
    val: Option<&Value>,
    root: &Value,
    seen: &mut HashSet<String>,
) -> Vec<Parameter> {
    let arr = match val.and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return Vec::new(),
    };

    arr.iter()
        .filter_map(|p| {
            let resolved = resolve_ref(p, root, seen).ok()?;
            let obj = resolved.as_object()?;
            let name = obj.get("name")?.as_str()?.to_string();
            let location = obj.get("in")?.as_str()?.to_string();
            let required = obj
                .get("required")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let description = obj
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let schema = obj
                .get("schema")
                .map(|s| extract_schema_info(s, root, seen, 0))
                .unwrap_or_else(default_schema);
            Some(Parameter {
                name,
                location,
                required,
                description,
                schema,
            })
        })
        .collect()
}

fn extract_schema_info(
    val: &Value,
    root: &Value,
    seen: &mut HashSet<String>,
    depth: usize,
) -> SchemaInfo {
    if depth > MAX_REF_DEPTH {
        return default_schema();
    }

    let resolved = resolve_ref(val, root, seen).unwrap_or(val);
    let obj = match resolved.as_object() {
        Some(o) => o,
        None => return default_schema(),
    };

    let type_name = obj
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("object")
        .to_string();

    let format = obj.get("format").and_then(|v| v.as_str()).map(String::from);

    let enum_values = obj.get("enum").and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|v| match v {
                Value::String(s) => Some(s.clone()),
                other => Some(other.to_string()),
            })
            .collect()
    });

    let description = obj
        .get("description")
        .and_then(|v| v.as_str())
        .map(String::from);
    let minimum = obj.get("minimum").and_then(|v| v.as_f64());
    let maximum = obj.get("maximum").and_then(|v| v.as_f64());
    let example = obj.get("example").cloned();

    let items_type = obj.get("items").and_then(|items| {
        let resolved_items = resolve_ref(items, root, seen).unwrap_or(items);
        resolved_items
            .get("type")
            .and_then(|v| v.as_str())
            .map(String::from)
    });

    SchemaInfo {
        type_name,
        format,
        enum_values,
        description,
        minimum,
        maximum,
        example,
        items_type,
    }
}

fn default_schema() -> SchemaInfo {
    SchemaInfo {
        type_name: "string".to_string(),
        format: None,
        enum_values: None,
        description: None,
        minimum: None,
        maximum: None,
        example: None,
        items_type: None,
    }
}

fn extract_request_body(
    val: Option<&Value>,
    root: &Value,
    seen: &mut HashSet<String>,
) -> Option<RequestBody> {
    let resolved = resolve_ref(val?, root, seen).ok()?;
    let obj = resolved.as_object()?;
    let required = obj
        .get("required")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let schema = obj
        .get("content")
        .and_then(|c| c.get("application/json"))
        .and_then(|j| j.get("schema"))?;

    let resolved_schema = resolve_ref(schema, root, seen).unwrap_or(schema);

    let fields = extract_object_fields(resolved_schema, root, seen);

    Some(RequestBody { required, fields })
}

fn extract_object_fields(
    schema: &Value,
    root: &Value,
    seen: &mut HashSet<String>,
) -> Vec<BodyField> {
    let obj = match schema.as_object() {
        Some(o) => o,
        None => return Vec::new(),
    };

    let required_set: HashSet<String> = obj
        .get("required")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let props = match obj.get("properties").and_then(|v| v.as_object()) {
        Some(p) => p,
        None => return Vec::new(),
    };

    props
        .iter()
        .map(|(name, prop_val)| {
            let resolved = resolve_ref(prop_val, root, seen).unwrap_or(prop_val);
            let schema_info = extract_schema_info(resolved, root, seen, 0);
            let description = schema_info
                .description
                .clone()
                .or_else(|| {
                    resolved
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                })
                .unwrap_or_default();
            BodyField {
                name: name.clone(),
                required: required_set.contains(name),
                description,
                schema: schema_info,
            }
        })
        .collect()
}

fn extract_responses(
    val: Option<&Value>,
    root: &Value,
    seen: &mut HashSet<String>,
) -> BTreeMap<String, Response> {
    let obj = match val.and_then(|v| v.as_object()) {
        Some(o) => o,
        None => return BTreeMap::new(),
    };

    let mut responses = BTreeMap::new();

    for (status, resp_val) in obj {
        let resolved = match resolve_ref(resp_val, root, seen) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let resp_obj = match resolved.as_object() {
            Some(o) => o,
            None => continue,
        };

        let description = resp_obj
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let fields = resp_obj
            .get("content")
            .and_then(|c| c.get("application/json"))
            .and_then(|j| j.get("schema"))
            .map(|schema| {
                let resolved_schema = resolve_ref(schema, root, seen).unwrap_or(schema);
                extract_object_fields(resolved_schema, root, seen)
            })
            .unwrap_or_default();

        responses.insert(
            status.clone(),
            Response {
                description,
                fields,
            },
        );
    }

    responses
}

fn detect_common_auth(
    header_counts: &HashMap<String, usize>,
    total_ops: u32,
    operations: &[Operation],
) -> Option<AuthInfo> {
    // A header that appears on >80% of operations is considered common auth
    let threshold = (total_ops as f64 * 0.8) as usize;

    let auth_header_names = ["Authorization", "authorization", "X-API-Key", "x-api-key"];

    for name in &auth_header_names {
        if let Some(&count) = header_counts.get(*name) {
            if count >= threshold {
                let description = operations
                    .iter()
                    .flat_map(|op| op.parameters.iter())
                    .find(|p| p.location == "header" && p.name == *name)
                    .map(|p| p.description.clone())
                    .unwrap_or_default();
                return Some(AuthInfo {
                    header_name: name.to_string(),
                    description,
                });
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_controller_style() {
        assert_eq!(
            clean_operation_name("SaleInvoiceController_getInvoice", "get", "/invoices"),
            "get_invoice"
        );
        assert_eq!(
            clean_operation_name("ItemsController_createItem", "post", "/items"),
            "create_item"
        );
    }

    #[test]
    fn clean_no_operation_id() {
        assert_eq!(
            clean_operation_name("", "get", "/api/items/{id}"),
            "get_api_items_id"
        );
    }

    #[test]
    fn snake_case() {
        assert_eq!(to_snake_case("getItems"), "get_items");
        assert_eq!(to_snake_case("createSaleInvoice"), "create_sale_invoice");
        assert_eq!(to_snake_case("getARAgingSummary"), "get_ar_aging_summary");
        assert_eq!(to_snake_case("list-rooms"), "list_rooms");
    }

    #[test]
    fn ref_cycle_detected() {
        let spec = serde_json::json!({
            "paths": {"/a": {"get": {
                "operationId": "test",
                "parameters": [{"$ref": "#/components/parameters/Loop"}]
            }}},
            "components": {"parameters": {
                "Loop": {"$ref": "#/components/parameters/Loop"}
            }}
        });
        let json = serde_json::to_vec(&spec).unwrap();
        let api = parse(&json).unwrap();
        // The cyclic parameter is silently dropped rather than crashing
        assert!(api.operations[0].parameters.is_empty());
    }

    #[test]
    fn minimal_spec() {
        let json = r#"{
            "openapi": "3.0.0",
            "paths": {
                "/items": {
                    "get": {
                        "operationId": "listItems",
                        "summary": "List all items",
                        "tags": ["Items"],
                        "parameters": [],
                        "responses": {
                            "200": {"description": "OK"}
                        }
                    }
                }
            }
        }"#;
        let api = parse(json.as_bytes()).unwrap();
        assert_eq!(api.operations.len(), 1);
        assert_eq!(api.operations[0].name, "list_items");
        assert_eq!(api.operations[0].summary, "List all items");
        assert_eq!(api.operations[0].method, "GET");
    }

    #[test]
    fn common_auth_detected() {
        let json = r#"{
            "openapi": "3.0.0",
            "paths": {
                "/a": {"get": {"operationId": "a", "parameters": [
                    {"name": "Authorization", "in": "header", "required": true,
                     "description": "Bearer token", "schema": {"type": "string"}}
                ]}},
                "/b": {"get": {"operationId": "b", "parameters": [
                    {"name": "Authorization", "in": "header", "required": true,
                     "description": "Bearer token", "schema": {"type": "string"}}
                ]}}
            }
        }"#;
        let api = parse(json.as_bytes()).unwrap();
        assert!(api.common_auth.is_some());
        assert_eq!(
            api.common_auth.as_ref().unwrap().header_name,
            "Authorization"
        );
        // Auth params should be stripped from operations
        for op in &api.operations {
            assert!(
                !op.parameters.iter().any(|p| p.name == "Authorization"),
                "Authorization should have been stripped"
            );
        }
    }
}

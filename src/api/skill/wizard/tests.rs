use super::*;

fn small_spec() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "openapi": "3.0.0",
        "info": {"title": "Test", "version": "1.0"},
        "paths": {
            "/items": {
                "get": {
                    "operationId": "listItems",
                    "summary": "List all items",
                    "tags": ["Items"],
                    "parameters": [
                        {"name": "page", "in": "query", "required": false,
                         "description": "Page number", "schema": {"type": "number"}},
                        {"name": "Authorization", "in": "header", "required": true,
                         "description": "Bearer token", "schema": {"type": "string"}}
                    ],
                    "responses": {
                        "200": {"description": "OK", "content": {
                            "application/json": {"schema": {
                                "type": "object",
                                "properties": {
                                    "items": {"type": "array", "description": "The items"},
                                    "total": {"type": "number", "description": "Total count"}
                                }
                            }}
                        }}
                    }
                },
                "post": {
                    "operationId": "ItemsController_createItem",
                    "summary": "Create a new item",
                    "tags": ["Items"],
                    "parameters": [
                        {"name": "Authorization", "in": "header", "required": true,
                         "description": "Bearer token", "schema": {"type": "string"}}
                    ],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {"schema": {
                                "type": "object",
                                "required": ["name"],
                                "properties": {
                                    "name": {"type": "string", "description": "Item name"},
                                    "price": {"type": "number", "description": "Price", "minimum": 0}
                                }
                            }}
                        }
                    },
                    "responses": {
                        "201": {"description": "Created"}
                    }
                }
            },
            "/items/{id}": {
                "get": {
                    "operationId": "getItem",
                    "summary": "Get one item",
                    "tags": ["Items"],
                    "parameters": [
                        {"name": "id", "in": "path", "required": true,
                         "description": "Item ID", "schema": {"type": "string"}},
                        {"name": "Authorization", "in": "header", "required": true,
                         "description": "Bearer token", "schema": {"type": "string"}}
                    ],
                    "responses": {
                        "200": {"description": "OK"},
                        "404": {"description": "Not found"}
                    }
                }
            }
        }
    }))
    .unwrap()
}

#[test]
fn flat_manifest_small_spec() {
    let input = WizardInput {
        spec_json: small_spec(),
        slug: "testapi".to_string(),
        base_url: "https://api.example.com".to_string(),
    };
    let output = generate(&input).unwrap();

    assert!(
        output.body.contains("`list_items`"),
        "body should have list_items: {}",
        output.body
    );
    assert!(
        output.body.contains("`create_item`"),
        "body should have create_item"
    );
    assert!(
        output.body.contains("`get_item`"),
        "body should have get_item"
    );

    assert_eq!(output.files.len(), 3);
    assert!(output.files.contains_key("list_items.md"));
    assert!(output.files.contains_key("create_item.md"));
    assert!(output.files.contains_key("get_item.md"));

    assert!(
        output
            .body
            .contains("Authentication is handled by the platform"),
        "body should mention auth: {}",
        output.body
    );
    let detail = &output.files["list_items.md"];
    assert!(
        detail.contains("attached by the platform"),
        "detail should mention platform auth: {detail}"
    );

    assert_eq!(output.hosts, vec!["api.example.com"]);
}

#[test]
fn detail_file_structure() {
    let input = WizardInput {
        spec_json: small_spec(),
        slug: "testapi".to_string(),
        base_url: "https://api.example.com".to_string(),
    };
    let output = generate(&input).unwrap();
    let detail = &output.files["create_item.md"];

    assert!(
        detail.starts_with("# create_item\n"),
        "starts with heading: {detail}"
    );
    assert!(detail.contains("## The call"), "has call section");
    assert!(
        detail.contains("POST https://api.example.com/items"),
        "has method and url: {detail}"
    );
    assert!(
        detail.contains("## What to send"),
        "has request body section"
    );
    assert!(detail.contains("`name`"), "has required field");
    assert!(detail.contains("`price`"), "has optional field");
    assert!(
        detail.contains("## What comes back"),
        "has response section"
    );
}

#[test]
fn detail_path_parameters() {
    let input = WizardInput {
        spec_json: small_spec(),
        slug: "testapi".to_string(),
        base_url: "https://api.example.com".to_string(),
    };
    let output = generate(&input).unwrap();
    let detail = &output.files["get_item.md"];

    assert!(
        detail.contains("/items/{id}"),
        "preserves path param in URL: {detail}"
    );
    assert!(detail.contains("`id`"), "documents path parameter");
}

#[test]
fn categories_for_large_spec() {
    let mut paths = serde_json::Map::new();
    for i in 0..40 {
        let tag = match i % 3 {
            0 => "Sales",
            1 => "Purchases",
            _ => "Reports",
        };
        let path_key = format!("/resource_{i}");
        let op = serde_json::json!({
            "get": {
                "operationId": format!("operation{i}"),
                "summary": format!("Operation {i}"),
                "tags": [tag],
                "parameters": [],
                "responses": {"200": {"description": "OK"}}
            }
        });
        paths.insert(path_key, op);
    }

    let spec = serde_json::json!({
        "openapi": "3.0.0",
        "info": {"title": "Big", "version": "1.0"},
        "paths": paths
    });

    let input = WizardInput {
        spec_json: serde_json::to_vec(&spec).unwrap(),
        slug: "big".to_string(),
        base_url: "https://big.example.com".to_string(),
    };
    let output = generate(&input).unwrap();

    assert!(
        output.files.contains_key("sales.md"),
        "should have sales category file, files: {:?}",
        output.files.keys().collect::<Vec<_>>()
    );
    assert!(output.files.contains_key("purchases.md"));
    assert!(output.files.contains_key("reports.md"));

    assert!(
        output.body.contains("**Sales**"),
        "body should list categories: {}",
        output.body
    );
    assert!(
        !output.body.contains("`operation0`"),
        "body should not list individual ops"
    );

    let sales = &output.files["sales.md"];
    assert!(
        sales.starts_with("# Sales\n"),
        "category starts with heading"
    );
    assert!(
        sales.contains("`operation0`"),
        "sales should contain operation0: {sales}"
    );

    assert_eq!(
        output.files.len(),
        40 + 3,
        "should have 40 detail files + 3 category files"
    );
}

#[test]
fn ref_resolution() {
    let spec = serde_json::json!({
        "openapi": "3.0.0",
        "paths": {
            "/things": {
                "post": {
                    "operationId": "createThing",
                    "summary": "Make a thing",
                    "tags": ["Things"],
                    "parameters": [],
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": {"$ref": "#/components/schemas/CreateThing"}
                            }
                        }
                    },
                    "responses": {
                        "201": {
                            "description": "Created",
                            "content": {
                                "application/json": {
                                    "schema": {"$ref": "#/components/schemas/Thing"}
                                }
                            }
                        }
                    }
                }
            }
        },
        "components": {
            "schemas": {
                "CreateThing": {
                    "type": "object",
                    "required": ["name"],
                    "properties": {
                        "name": {"type": "string", "description": "Thing name"},
                        "weight": {"type": "number", "description": "Weight in kg"}
                    }
                },
                "Thing": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string", "description": "Unique ID"},
                        "name": {"type": "string"},
                        "weight": {"type": "number"}
                    }
                }
            }
        }
    });

    let input = WizardInput {
        spec_json: serde_json::to_vec(&spec).unwrap(),
        slug: "things".to_string(),
        base_url: "https://things.example.com".to_string(),
    };
    let output = generate(&input).unwrap();
    let detail = &output.files["create_thing.md"];

    assert!(
        detail.contains("`name`"),
        "should resolve $ref for request: {detail}"
    );
    assert!(
        detail.contains("`id`"),
        "should resolve $ref for response: {detail}"
    );
}

#[test]
fn enum_values_rendered() {
    let spec = serde_json::json!({
        "openapi": "3.0.0",
        "paths": {
            "/items": {
                "get": {
                    "operationId": "listItems",
                    "summary": "List items",
                    "parameters": [
                        {"name": "sort", "in": "query", "required": false,
                         "description": "Sort order",
                         "schema": {"type": "string", "enum": ["asc", "desc"]}}
                    ],
                    "responses": {"200": {"description": "OK"}}
                }
            }
        }
    });

    let input = WizardInput {
        spec_json: serde_json::to_vec(&spec).unwrap(),
        slug: "test".to_string(),
        base_url: "https://test.example.com".to_string(),
    };
    let output = generate(&input).unwrap();
    let detail = &output.files["list_items.md"];

    assert!(
        detail.contains("`asc`") && detail.contains("`desc`"),
        "should render enum values: {detail}"
    );
}

#[test]
fn host_extraction() {
    assert_eq!(
        extract_host("https://books.idataconnect.com/api"),
        "books.idataconnect.com"
    );
    assert_eq!(extract_host("http://localhost:8080"), "localhost:8080");
    assert_eq!(extract_host("https://api.example.com"), "api.example.com");
}

#[test]
fn slugify_tags() {
    assert_eq!(slugify("Sale Invoices"), "sale_invoices");
    assert_eq!(slugify("Banking Plaid"), "banking_plaid");
    assert_eq!(slugify("Api keys"), "api_keys");
    assert_eq!(
        slugify("Credit Notes Apply Invoice"),
        "credit_notes_apply_invoice"
    );
}

#[test]
fn too_large_rejected() {
    let input = WizardInput {
        spec_json: vec![0u8; 17 * 1024 * 1024],
        slug: "x".to_string(),
        base_url: "https://x.com".to_string(),
    };
    assert!(matches!(generate(&input), Err(ParseError::TooLarge(_))));
}

#[test]
fn empty_paths_rejected() {
    let spec = serde_json::json!({"openapi": "3.0.0", "paths": {}});
    let input = WizardInput {
        spec_json: serde_json::to_vec(&spec).unwrap(),
        slug: "x".to_string(),
        base_url: "https://x.com".to_string(),
    };
    assert!(matches!(generate(&input), Err(ParseError::NoOperations)));
}

#[test]
fn files_within_read_budget() {
    let input = WizardInput {
        spec_json: small_spec(),
        slug: "testapi".to_string(),
        base_url: "https://api.example.com".to_string(),
    };
    let output = generate(&input).unwrap();

    for (path, content) in &output.files {
        assert!(
            content.len() < 32 * 1024,
            "file {path} is {} bytes, must be under 32KB",
            content.len()
        );
    }
    assert!(
        output.body.len() < 32 * 1024,
        "body is {} bytes, must be under 32KB",
        output.body.len()
    );
}

#[test]
fn manifest_says_read_object() {
    let input = WizardInput {
        spec_json: small_spec(),
        slug: "testapi".to_string(),
        base_url: "https://api.example.com".to_string(),
    };
    let output = generate(&input).unwrap();

    assert!(
        output.body.contains("read_object"),
        "manifest must tell the agent to use read_object: {}",
        output.body
    );
}

#[test]
fn manifest_no_urls() {
    let input = WizardInput {
        spec_json: small_spec(),
        slug: "testapi".to_string(),
        base_url: "https://api.example.com".to_string(),
    };
    let output = generate(&input).unwrap();

    assert!(
        !output.body.contains("https://"),
        "manifest must not contain URLs (names not URLs): {}",
        output.body
    );
}

#[test]
fn detail_says_fetch_url() {
    let input = WizardInput {
        spec_json: small_spec(),
        slug: "testapi".to_string(),
        base_url: "https://api.example.com".to_string(),
    };
    let output = generate(&input).unwrap();
    let detail = &output.files["list_items.md"];

    assert!(
        detail.contains("fetch_url"),
        "detail must tell agent to use fetch_url: {detail}"
    );
}

#[test]
#[cfg_attr(not(feature = "slow-tests"), ignore)]
fn bigcapital_spec() {
    let spec = std::fs::read("tests/fixtures/bigcapital-openapi.json").unwrap();
    let input = WizardInput {
        spec_json: spec,
        slug: "bigcapital".to_string(),
        base_url: "https://books.idataconnect.com".to_string(),
    };
    let output = generate(&input).unwrap();

    // Should produce categories (383 operations >> 30 threshold)
    let body = &output.body;
    assert!(
        body.contains("**Sale Invoices**"),
        "body should have Sale Invoices category: {body}"
    );
    assert!(
        body.contains("**Items**"),
        "body should have Items category"
    );

    // Every detail file is under 32KB
    for (path, content) in &output.files {
        assert!(
            content.len() < 32 * 1024,
            "file {path} is {} bytes, must be under 32KB for a single read_object",
            content.len()
        );
    }

    // Body is under 32KB
    assert!(
        output.body.len() < 32 * 1024,
        "body is {} bytes, must be under 32KB",
        output.body.len()
    );

    // Auth should be detected (Authorization header is on every operation)
    assert!(
        body.contains("Authentication is handled by the platform"),
        "auth should be factored out: {body}"
    );

    // Host extracted
    assert_eq!(output.hosts, vec!["books.idataconnect.com"]);

    // Spot-check a few detail files
    let create_item_key = output
        .files
        .keys()
        .find(|k| k.contains("create_item"))
        .expect("should have a create_item operation");
    let detail = &output.files[create_item_key];
    assert!(detail.contains("fetch_url"), "detail should use fetch_url");
    assert!(
        detail.contains("books.idataconnect.com"),
        "detail should have base URL"
    );

    // Print summary for manual inspection
    eprintln!(
        "Generated {} files + body ({} bytes)",
        output.files.len(),
        output.body.len()
    );
    eprintln!(
        "Body preview:\n{}",
        &output.body[..output.body.len().min(2000)]
    );

    eprintln!(
        "Generated {} files + body ({} bytes)",
        output.files.len(),
        output.body.len()
    );
}

#[test]
fn references_name_the_path_a_guest_reads() {
    let input = WizardInput {
        spec_json: small_spec(),
        slug: "testapi".to_string(),
        base_url: "https://api.example.com".to_string(),
    };
    let output = generate(&input).unwrap();
    assert!(output.files.contains_key("list_items.md"));
    assert!(
        output.body.contains("`skill/testapi/list_items.md`"),
        "body: {}",
        output.body
    );
}

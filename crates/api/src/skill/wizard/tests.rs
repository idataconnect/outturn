use super::*;
use outturn_core::egress::policy::Risk;

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
        auth_header: Some("Authorization".into()),
        annotations: Vec::new(),
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
        auth_header: None,
        annotations: Vec::new(),
    };
    let output = generate(&input).unwrap();
    let detail = &output.files["create_item.md"];

    // The operation is declared, so the frontmatter comes first and the
    // prose starts after it.
    let parsed = crate::skill::parse(detail).unwrap();
    assert!(
        parsed.body.trim_start().starts_with("# create_item\n"),
        "starts with heading: {detail}"
    );
    assert_eq!(
        parsed.operation.expect("declared").url,
        "https://api.example.com/items"
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
        auth_header: None,
        annotations: Vec::new(),
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
        auth_header: None,
        annotations: Vec::new(),
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
        auth_header: None,
        annotations: Vec::new(),
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
        auth_header: None,
        annotations: Vec::new(),
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
        extract_host("https://books.example.com/api"),
        "books.example.com"
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
        auth_header: None,
        annotations: Vec::new(),
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
        auth_header: None,
        annotations: Vec::new(),
    };
    assert!(matches!(generate(&input), Err(ParseError::NoOperations)));
}

#[test]
fn files_within_read_budget() {
    let input = WizardInput {
        spec_json: small_spec(),
        slug: "testapi".to_string(),
        base_url: "https://api.example.com".to_string(),
        auth_header: None,
        annotations: Vec::new(),
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
        auth_header: None,
        annotations: Vec::new(),
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
        auth_header: None,
        annotations: Vec::new(),
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
        auth_header: None,
        annotations: Vec::new(),
    };
    let output = generate(&input).unwrap();
    // Every operation here is a tool, so none spells out a call.
    let detail = &output.files["list_items.md"];
    assert!(!detail.contains("fetch_url"), "{detail}");

    // One that cannot be declared still says how to make the call by hand.
    let output = generate(&shop_input()).unwrap();
    let detail = &output.files["create_order.md"];
    assert!(
        detail.contains("fetch_url"),
        "detail must tell agent to use fetch_url: {detail}"
    );
}

#[test]
#[cfg_attr(not(feature = "slow-tests"), ignore)]
fn bigcapital_spec() {
    let spec = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/bigcapital-openapi.json"
    ))
    .unwrap();
    let input = WizardInput {
        spec_json: spec,
        slug: "bigcapital".to_string(),
        base_url: "https://books.example.com".to_string(),
        auth_header: Some("Authorization".into()),
        annotations: Vec::new(),
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
    assert_eq!(output.hosts, vec!["books.example.com"]);

    // Spot-check a few detail files
    let create_item_key = output
        .files
        .keys()
        .find(|k| k.contains("create_item"))
        .expect("should have a create_item operation");
    let detail = &output.files[create_item_key];
    assert!(
        detail.contains("books.example.com"),
        "detail should have base URL"
    );

    // Its organization header is marked required on nearly every operation,
    // and is only needed with a JWT; an API key needs none. That mark must not
    // keep the operations people use from being tools.
    for name in [
        "get_customers.md",
        "get_sale_invoices.md",
        "create_customer.md",
    ] {
        let parsed = crate::skill::parse(&output.files[name]).unwrap();
        assert!(parsed.operation.is_some(), "{name} should be a tool");
    }
    let declared = output
        .files
        .values()
        .filter(|f| crate::skill::parse(f).is_ok_and(|p| p.operation.is_some()))
        .count();
    eprintln!("{declared} operations declared as tools");
    let largest = output.files.iter().max_by_key(|(_, f)| f.len()).unwrap();
    eprintln!("largest file {} at {} bytes", largest.0, largest.1.len());
    // Every file has to publish: a version refuses one past what an agent
    // reads in one call.
    for (name, file) in &output.files {
        assert!(
            file.len() <= crate::skill::files::MAX_FILE_BYTES,
            "{name} is {} bytes",
            file.len()
        );
    }

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
        auth_header: None,
        annotations: Vec::new(),
    };
    let output = generate(&input).unwrap();
    assert!(output.files.contains_key("list_items.md"));
    assert!(
        output.body.contains("`skill/testapi/list_items.md`"),
        "body: {}",
        output.body
    );
}

fn spec_with(extra: serde_json::Value) -> Vec<u8> {
    let mut spec: serde_json::Value = serde_json::from_slice(&small_spec()).unwrap();
    for (k, v) in extra.as_object().unwrap() {
        spec[k] = v.clone();
    }
    serde_json::to_vec(&spec).unwrap()
}

#[test]
fn preview_reads_what_the_specification_declares() {
    let spec = spec_with(serde_json::json!({
        "info": {"title": "Acme Billing API", "description": "Invoices.", "version": "1"},
        "servers": [{"url": "https://api.acme.test/v2/"}],
        "components": {"securitySchemes": {"key": {"type": "apiKey", "in": "header", "name": "X-Acme-Key"}}},
    }));
    let p = preview(&spec, None).unwrap();
    assert_eq!(p.name, "Acme Billing API");
    assert_eq!(p.description, "Invoices.");
    assert_eq!(p.slug, "acme-billing-api");
    assert_eq!(p.credential_env, "OUTTURN_EGRESS_ACME_BILLING_API_KEY");
    assert_eq!(p.base_url.as_deref(), Some("https://api.acme.test/v2"));
    assert_eq!(p.auth_header.as_deref(), Some("X-Acme-Key"));
    assert_eq!(p.operations, 3);
    assert_eq!(p.categories, 1);
}

#[test]
fn preview_names_authorization_for_a_bearer_scheme() {
    let spec = spec_with(serde_json::json!({
        "components": {"securitySchemes": {
            "q": {"type": "apiKey", "in": "query", "name": "key"},
            "b": {"type": "http", "scheme": "bearer"},
        }},
        "security": [{"b": []}],
    }));
    assert_eq!(
        preview(&spec, None).unwrap().auth_header.as_deref(),
        Some("Authorization")
    );
}

#[test]
fn preview_falls_back_to_a_header_every_operation_takes() {
    // small_spec declares no scheme, only an Authorization parameter on each
    // operation, which is what Bigcapital does too.
    let p = preview(&small_spec(), None).unwrap();
    assert_eq!(p.auth_header.as_deref(), Some("Authorization"));
    assert_eq!(p.slug, "test");
    assert_eq!(p.credential_env, "OUTTURN_EGRESS_TEST_API_KEY");
}

#[test]
fn preview_resolves_a_relative_server_against_the_fetch_url() {
    let spec = spec_with(serde_json::json!({"servers": [{"url": "/api/v1"}]}));
    assert_eq!(preview(&spec, None).unwrap().base_url, None);
    let p = preview(&spec, Some("https://docs.acme.test/spec/openapi.json")).unwrap();
    assert_eq!(p.base_url.as_deref(), Some("https://docs.acme.test/api/v1"));
}

#[test]
fn preview_substitutes_server_variable_defaults() {
    let spec = spec_with(serde_json::json!({"servers": [{
        "url": "https://{region}.acme.test",
        "variables": {"region": {"default": "eu"}},
    }]}));
    assert_eq!(
        preview(&spec, None).unwrap().base_url.as_deref(),
        Some("https://eu.acme.test")
    );
}

#[test]
fn preview_leaves_a_missing_server_unanswered() {
    let spec = spec_with(serde_json::json!({"servers": []}));
    assert_eq!(preview(&spec, None).unwrap().base_url, None);
}

/// Two schemes, declared rather than taken as parameters: what the skill says
/// about auth is the header the operator confirmed, not a second guess.
fn petstore_spec() -> Vec<u8> {
    let op = |id: &str, security: Option<serde_json::Value>| {
        let mut o = serde_json::json!({
            "operationId": id,
            "summary": id,
            "tags": ["pet"],
            "parameters": [{"name": "api_key", "in": "header", "required": false,
                            "schema": {"type": "string"}}],
            "responses": {"200": {"description": "OK"}}
        });
        if let Some(s) = security {
            o["security"] = s;
        }
        o
    };
    let oauth = || Some(serde_json::json!([{"petstore_auth": ["write:pets"]}]));
    serde_json::to_vec(&serde_json::json!({
        "openapi": "3.0.0",
        "info": {"title": "Swagger Petstore", "version": "1"},
        "servers": [{"url": "https://petstore3.swagger.io/api/v3"}],
        "components": {"securitySchemes": {
            "api_key": {"type": "apiKey", "in": "header", "name": "api_key"},
            "petstore_auth": {"type": "oauth2", "flows": {"implicit": {
                "authorizationUrl": "https://petstore3.swagger.io/oauth/authorize",
                "scopes": {}}}},
        }},
        "paths": {
            "/pet": {"put": op("updatePet", oauth()), "post": op("addPet", oauth())},
            "/pet/findByStatus": {"get": op("findPetsByStatus", oauth())},
            "/pet/findByTags": {"get": op("findPetsByTags", oauth())},
            "/pet/{petId}": {
                "get": op("getPetById", Some(serde_json::json!([{"api_key": []}, {"petstore_auth": []}]))),
                "post": op("updatePetWithForm", oauth()),
                "delete": op("deletePet", oauth()),
            },
            "/pet/{petId}/uploadImage": {"post": op("uploadFile", oauth())},
            "/store/inventory": {"get": op("getInventory", Some(serde_json::json!([{"api_key": []}])))},
            "/store/order": {"post": op("placeOrder", None)},
            "/store/order/{orderId}": {"get": op("getOrderById", None), "delete": op("deleteOrder", None)},
            "/user": {"post": op("createUser", None)},
            "/user/login": {"get": op("loginUser", None)},
            "/user/logout": {"get": op("logoutUser", None)},
            "/user/{username}": {
                "get": op("getUserByName", None),
                "put": op("updateUser", None),
                "delete": op("deleteUser", None),
            },
            "/user/createWithList": {"post": op("createUsersWithListInput", None)},
        }
    }))
    .unwrap()
}

#[test]
fn generate_uses_the_header_it_is_given() {
    let input = WizardInput {
        spec_json: petstore_spec(),
        slug: "petstore".to_string(),
        base_url: "https://petstore3.swagger.io/api/v3".to_string(),
        auth_header: Some("API_KEY".into()),
        annotations: Vec::new(),
    };
    let output = generate(&input).unwrap();
    assert!(
        output.body.contains("the `API_KEY` header is attached"),
        "{}",
        output.body
    );
    let detail = &output.files["get_inventory.md"];
    assert!(detail.contains("`API_KEY` header is attached by the platform"));
    // Stripped whatever its case, since the platform sets it.
    assert!(
        !detail.contains("`api_key`"),
        "the credential header is not the model's to set: {detail}"
    );
}

#[test]
fn generate_says_nothing_about_auth_when_given_no_header() {
    // small_spec takes Authorization on every operation; with no header
    // confirmed, generate does not guess one.
    let input = WizardInput {
        spec_json: small_spec(),
        slug: "testapi".to_string(),
        base_url: "https://api.example.com".to_string(),
        auth_header: Some("  ".into()),
        annotations: Vec::new(),
    };
    let output = generate(&input).unwrap();
    assert!(!output.body.contains("Authentication is handled"));
    assert!(output.files["list_items.md"].contains("Authorization"));
}

#[test]
fn preview_chooses_the_scheme_operations_use() {
    let spec = spec_with(serde_json::json!({
        "components": {"securitySchemes": {
            "a_key": {"type": "apiKey", "in": "header", "name": "X-Rarely"},
            "b_bearer": {"type": "http", "scheme": "bearer"},
        }},
        "paths": {
            "/a": {"get": {"security": [{"a_key": []}], "responses": {}},
                   "put": {"security": [{"b_bearer": []}], "responses": {}}},
            "/b": {"get": {"security": [{"b_bearer": []}], "responses": {}},
                   "post": {"security": [{"b_bearer": []}, {"a_key": []}], "responses": {}}},
        },
    }));
    assert_eq!(
        preview(&spec, None).unwrap().auth_header.as_deref(),
        Some("Authorization")
    );
}

#[test]
fn preview_inherits_root_security_for_operations_declaring_none() {
    let spec = spec_with(serde_json::json!({
        "components": {"securitySchemes": {
            "a_key": {"type": "apiKey", "in": "header", "name": "X-Key"},
            "b_basic": {"type": "http", "scheme": "basic"},
        }},
        "security": [{"a_key": []}],
        "paths": {
            "/a": {"get": {"responses": {}}, "post": {"responses": {}}},
            "/b": {"get": {"security": [{"b_basic": []}], "responses": {}}},
        },
    }));
    assert_eq!(
        preview(&spec, None).unwrap().auth_header.as_deref(),
        Some("X-Key")
    );
}

#[test]
fn preview_passes_over_the_petstores_oauth_for_its_api_key() {
    // OAuth on seven operations, but a rule cannot serve it; the key, on two,
    // it can.
    let p = preview(&petstore_spec(), None).unwrap();
    assert_eq!(p.auth_header.as_deref(), Some("api_key"));
}

#[test]
fn preview_says_what_the_petstore_cannot_serve() {
    let p = preview(&petstore_spec(), None).unwrap();
    assert_eq!(p.auth_kind, Some(Kind::ApiKeyHeader));
    // getPetById takes either, so only the seven OAuth-only operations.
    assert_eq!(p.unserved_operations, 7);
    assert_eq!(p.unserved_kinds, vec![Kind::OAuth2]);
}

#[test]
fn preview_serves_nothing_from_a_query_key() {
    let spec = spec_with(serde_json::json!({
        "components": {"securitySchemes": {
            "q": {"type": "apiKey", "in": "query", "name": "key"},
            "oidc": {"type": "openIdConnect", "openIdConnectUrl": "https://id.test/.well-known"},
        }},
        "security": [{"q": []}, {"oidc": []}],
    }));
    let p = preview(&spec, None).unwrap();
    assert_eq!(p.auth_kind, None);
    assert_eq!(p.unserved_operations, 3);
    assert_eq!(
        p.unserved_kinds,
        vec![Kind::ApiKeyQuery, Kind::OpenIdConnect]
    );
    // An alternative needing a key alongside the header is not served by it.
    let both = spec_with(serde_json::json!({
        "components": {"securitySchemes": {
            "h": {"type": "apiKey", "in": "header", "name": "X-Key"},
            "c": {"type": "apiKey", "in": "cookie", "name": "sid"},
        }},
        "security": [{"h": [], "c": []}],
    }));
    let p = preview(&both, None).unwrap();
    assert_eq!(p.auth_header.as_deref(), Some("X-Key"));
    assert_eq!(p.unserved_operations, 3);
    assert_eq!(p.unserved_kinds, vec![Kind::ApiKeyCookie]);
    assert_eq!(
        serde_json::to_value(Kind::OAuth2).unwrap(),
        serde_json::json!("oauth2")
    );
}

fn yaml_input(spec: &str) -> WizardInput {
    WizardInput {
        spec_json: spec.as_bytes().to_vec(),
        slug: "x".to_string(),
        base_url: "https://x.com".to_string(),
        auth_header: None,
        annotations: Vec::new(),
    }
}

const SMALL_YAML: &str = "\
openapi: 3.0.0
info:
  title: Acme Billing
  description: Invoices and the like.
paths:
  /invoices/{id}:
    get:
      operationId: getInvoice
      summary: Fetch an invoice
      parameters:
        - name: id
          in: path
          required: true
          schema:
            type: string
";

#[test]
fn yaml_reads_as_the_same_document_as_json() {
    let from_yaml = document(SMALL_YAML.as_bytes()).unwrap();
    let from_json = document(&serde_json::to_vec(&from_yaml).unwrap()).unwrap();
    assert_eq!(from_yaml, from_json);
    assert_eq!(from_yaml["info"]["title"], "Acme Billing");
    // Unquoted 3.0.0 stays a string rather than becoming a number or failing.
    assert_eq!(from_yaml["openapi"], "3.0.0");
}

#[test]
fn yaml_specification_generates_and_previews() {
    let out = generate(&yaml_input(SMALL_YAML)).unwrap();
    assert!(
        out.files.keys().any(|k| k.contains("invoice")),
        "{:?}",
        out.files.keys()
    );
    let p = preview(SMALL_YAML.as_bytes(), None).unwrap();
    assert_eq!(p.name, "Acme Billing");
    assert_eq!(p.operations, 1);
}

#[test]
fn json_still_reads_as_json() {
    // JSON is also YAML, so the order matters only for the error; a JSON
    // document must never reach the YAML reader's rules about it.
    assert!(
        generate(&WizardInput {
            spec_json: small_spec(),
            ..yaml_input("")
        })
        .is_ok()
    );
}

#[test]
fn yaml_alias_bomb_refused() {
    // Nine levels of tenfold aliases: a few hundred bytes that would expand
    // to a billion strings.
    let mut bomb = String::from(
        "a: &a [\"lol\",\"lol\",\"lol\",\"lol\",\"lol\",\"lol\",\"lol\",\"lol\",\"lol\",\"lol\"]\n",
    );
    for (prev, name) in ["a", "b", "c", "d", "e", "f", "g", "h"]
        .iter()
        .zip(["b", "c", "d", "e", "f", "g", "h", "i"])
    {
        bomb.push_str(&format!(
            "{name}: &{name} [{}]\n",
            vec![format!("*{prev}"); 10].join(",")
        ));
    }
    bomb.push_str("paths: *i\n");
    assert!(bomb.len() < 1024);
    assert!(matches!(
        document(bomb.as_bytes()),
        Err(ParseError::Unreadable { .. })
    ));
    assert!(matches!(
        generate(&yaml_input(&bomb)),
        Err(ParseError::Unreadable { .. })
    ));
    assert!(preview(bomb.as_bytes(), None).is_err());
}

#[test]
fn any_yaml_alias_refused() {
    // Refused outright rather than budgeted, so even a harmless one is.
    let spec = format!("{SMALL_YAML}x-shared: &s {{type: string}}\nx-again: *s\n");
    let err = document(spec.as_bytes()).unwrap_err();
    assert!(matches!(err, ParseError::Unreadable { .. }), "{err}");
}

#[test]
fn too_large_yaml_rejected_before_parsing() {
    let big = "a: b\n".repeat(17 * 1024 * 1024 / 5 + 1);
    assert!(matches!(
        generate(&yaml_input(&big)),
        Err(ParseError::TooLarge(_))
    ));
    assert!(matches!(
        preview(big.as_bytes(), None),
        Err(ParseError::TooLarge(_))
    ));
}

#[test]
fn neither_json_nor_yaml_says_both() {
    let err = document(b"{ not: [ closed").unwrap_err();
    let text = err.to_string();
    assert!(text.contains("JSON") && text.contains("YAML"), "{text}");
}

mod annotations {
    use super::super::{Annotation, AnnotationKind, Target, WizardInput, generate};
    use super::small_spec;

    fn with(spec: Vec<u8>, annotations: Vec<Annotation>) -> super::super::WizardOutput {
        generate(&WizardInput {
            spec_json: spec,
            slug: "shop".into(),
            base_url: "https://api.shop.example".into(),
            auth_header: Some("Authorization".into()),
            annotations,
        })
        .unwrap()
    }

    fn note(target: Target, text: &str) -> Annotation {
        Annotation {
            target,
            kind: AnnotationKind::Note(text.into()),
        }
    }

    /// Thirty-one operations under two tags, so the skill has category files.
    fn big_spec() -> Vec<u8> {
        let mut paths = serde_json::Map::new();
        for i in 0..31 {
            let tag = if i < 16 { "Sale Invoices" } else { "Bills" };
            paths.insert(
                format!("/things/{i}"),
                serde_json::json!({"get": {"operationId": format!("op{i}"), "summary": "Do it", "tags": [tag],
                    "responses": {"200": {"description": "ok"}}}}),
            );
        }
        serde_json::to_vec(&serde_json::json!({
            "openapi": "3.0.0", "info": {"title": "Big", "version": "1"}, "paths": paths,
        }))
        .unwrap()
    }

    /// The skill level is the body, which every turn carries: a vocabulary
    /// note there is seen before the agent knows which category to open.
    #[test]
    fn a_skill_note_is_in_the_body() {
        let out = with(
            big_spec(),
            vec![note(
                Target::Skill,
                "Invoices are called sale invoices here.",
            )],
        );
        assert!(
            out.body
                .contains("- Invoices are called sale invoices here."),
            "{}",
            out.body
        );
        assert!(out.unmatched.is_empty());
    }

    #[test]
    fn a_category_note_is_in_its_file_and_not_the_body() {
        let out = with(
            big_spec(),
            vec![note(
                Target::Category("Sale Invoices".into()),
                "These are service invoices.",
            )],
        );
        assert!(out.files["sale_invoices.md"].contains("- These are service invoices."));
        assert!(!out.body.contains("service invoices"));
        assert!(!out.files["bills.md"].contains("service invoices"));
    }

    #[test]
    fn an_operation_note_preference_and_gate_are_in_its_file() {
        let base = with(small_spec(), Vec::new());
        let mut names: Vec<_> = base
            .files
            .keys()
            .map(|k| k.trim_end_matches(".md").to_string())
            .collect();
        names.sort();
        let (first, second) = (names[0].clone(), names[1].clone());
        let out = with(
            small_spec(),
            vec![
                note(Target::Operation(first.clone()), "Always pass a page."),
                Annotation {
                    target: Target::Operation(first.clone()),
                    kind: AnnotationKind::Prefer(second.clone()),
                },
                Annotation {
                    target: Target::Operation(first.clone()),
                    kind: AnnotationKind::Approval(
                        "approval:\n  requires: list\n  matches: GET /items\n  binds: [page]"
                            .into(),
                    ),
                },
            ],
        );
        let file = &out.files[&format!("{first}.md")];
        assert!(file.starts_with("---\napproval:"), "{file}");
        assert!(file.contains("- Always pass a page."));
        assert!(file.contains(&format!("**Prefer `{second}`**")));
        assert!(
            out.body.contains(&format!("Prefer `{second}`.")),
            "the choice is made in the list"
        );
        assert!(
            crate::skill::parse(file).unwrap().approval.is_some(),
            "the gate parses"
        );
    }

    #[test]
    fn a_hidden_operation_is_left_out_everywhere() {
        let base = with(small_spec(), Vec::new());
        let name = base
            .files
            .keys()
            .next()
            .unwrap()
            .trim_end_matches(".md")
            .to_string();
        let out = with(
            small_spec(),
            vec![Annotation {
                target: Target::Operation(name.clone()),
                kind: AnnotationKind::Hidden,
            }],
        );
        assert!(!out.files.contains_key(&format!("{name}.md")));
        assert!(!out.body.contains(&format!("`{name}`")));
    }

    /// A gate lives only in its operation's file, so hiding a gated operation
    /// would remove the gate while the host stays allowed. The gate wins.
    #[test]
    fn a_gated_operation_is_never_hidden() {
        let base = with(small_spec(), Vec::new());
        let name = base
            .files
            .keys()
            .next()
            .unwrap()
            .trim_end_matches(".md")
            .to_string();
        let out = with(
            small_spec(),
            vec![
                Annotation {
                    target: Target::Operation(name.clone()),
                    kind: AnnotationKind::Hidden,
                },
                Annotation {
                    target: Target::Operation(name.clone()),
                    kind: AnnotationKind::Approval(
                        "approval:\n  requires: x\n  matches: POST /x\n  binds: [a]".into(),
                    ),
                },
            ],
        );
        assert!(out.files[&format!("{name}.md")].starts_with("---\napproval:"));
    }

    /// The specification changed under an annotation: it is reported, not
    /// dropped and not rendered somewhere it no longer means anything.
    #[test]
    fn an_annotation_whose_target_is_gone_is_reported() {
        let out = with(
            small_spec(),
            vec![
                note(Target::Operation("renamed_away".into()), "x"),
                note(Target::Category("Nope".into()), "y"),
                note(Target::Skill, "z"),
            ],
        );
        assert_eq!(out.unmatched, vec![0, 1]);
        assert!(!out.body.contains("- x") && !out.body.contains("- y"));
    }
}

// -- Operations as tools ------------------------------------------------------
//
// See docs/skill-operations.md. An operation the frontmatter can describe is
// declared, so the agent is offered it as a tool; one it cannot is written as
// before, with the call spelled out for `fetch_url`.

fn small_input(annotations: Vec<Annotation>) -> WizardInput {
    WizardInput {
        spec_json: small_spec(),
        slug: "testapi".to_string(),
        base_url: "https://api.example.com".to_string(),
        auth_header: Some("Authorization".into()),
        annotations,
    }
}

#[test]
fn an_operation_the_frontmatter_can_say_is_declared() {
    let output = generate(&small_input(Vec::new())).unwrap();

    let create = crate::skill::parse(&output.files["create_item.md"])
        .unwrap()
        .operation
        .expect("create_item is a tool");
    assert_eq!(create.method, "POST");
    assert_eq!(create.url, "https://api.example.com/items");
    assert_eq!(create.body, ["name", "price"]);
    let args = create.arguments_schema();
    assert_eq!(args["required"], serde_json::json!(["name"]));
    assert_eq!(args["properties"]["price"]["type"], "number");
    assert_eq!(create.summary, "Create a new item");

    let get = crate::skill::parse(&output.files["get_item.md"])
        .unwrap()
        .operation
        .expect("get_item is a tool");
    assert_eq!(get.url, "https://api.example.com/items/{id}");
    assert_eq!(get.path_params(), ["id"]);

    let list = crate::skill::parse(&output.files["list_items.md"])
        .unwrap()
        .operation
        .expect("list_items is a tool");
    assert_eq!(list.query, ["page"]);
    assert!(
        list.arguments_schema().get("required").is_none(),
        "page is optional"
    );
}

#[test]
fn a_declared_operation_does_not_spell_out_its_call() {
    // The tool builds the request; a URL in the prose would be a second copy
    // for a model to compose from by hand.
    let output = generate(&small_input(Vec::new())).unwrap();
    let file = &output.files["create_item.md"];
    assert!(!file.contains("## The call"), "{file}");
    assert!(!file.contains("fetch_url"), "{file}");
    // What to send stays: it is what the arguments mean.
    assert!(file.contains("## What to send"), "{file}");
    assert!(output.body.contains("load_tools"), "{}", output.body);
    assert!(output.body.contains("tool of its own"), "{}", output.body);
}

#[test]
fn an_operation_the_frontmatter_cannot_say_keeps_its_call() {
    // A body that is a bare array is not an object of arguments, so the
    // operation is left to prose and `fetch_url` rather than offered with the
    // wrong shape.
    let output = generate(&shop_input()).unwrap();

    let create = &output.files["create_order.md"];
    assert!(
        create.contains("Content-Type: application/json"),
        "a JSON body sent by hand says so: {create}"
    );
    assert!(
        crate::skill::parse(create).unwrap().operation.is_none(),
        "{create}"
    );
    assert!(create.contains("## The call"), "{create}");
    assert!(create.contains("fetch_url"), "{create}");
    assert!(
        crate::skill::parse(&output.files["get_order.md"])
            .unwrap()
            .operation
            .is_some()
    );
    assert!(output.body.contains("Some operations"), "{}", output.body);
}

#[test]
fn an_approval_rule_and_its_operation_share_the_frontmatter() {
    let output = generate(&small_input(vec![Annotation {
        target: Target::Operation("create_item".into()),
        kind: AnnotationKind::Approval(
            "approval:\n  requires: create\n  matches: POST /items\n  binds: [name, price]".into(),
        ),
    }]))
    .unwrap();
    let parsed = crate::skill::parse(&output.files["create_item.md"]).unwrap();
    assert!(parsed.approval.is_some(), "the gate is kept");
    assert!(parsed.operation.is_some(), "and the operation beside it");
}

#[test]
fn an_approval_rule_that_does_not_fit_leaves_the_operation_undeclared() {
    // The person's rule is kept as written; declaring the operation beside it
    // would make a file publishing refuses, so the call is spelled out instead.
    let output = generate(&small_input(vec![Annotation {
        target: Target::Operation("create_item".into()),
        kind: AnnotationKind::Approval(
            "approval:\n  requires: create\n  matches: POST /things\n  binds: [name]".into(),
        ),
    }]))
    .unwrap();
    let file = &output.files["create_item.md"];
    let parsed = crate::skill::parse(file).unwrap();
    assert!(parsed.approval.is_some());
    assert!(parsed.operation.is_none(), "{file}");
    assert!(file.contains("## The call"), "{file}");
}

/// A shop whose order is placed with a bare JSON array, which is not an object
/// of arguments, so placing one cannot be declared -- and an order read by id,
/// which can.
fn shop_input() -> WizardInput {
    let spec = serde_json::to_vec(&serde_json::json!({
        "openapi": "3.0.0",
        "info": {"title": "Test", "version": "1.0"},
        "paths": {
            "/orders": {"post": {
                "operationId": "createOrder",
                "summary": "Place an order",
                "requestBody": {"required": true, "content": {"application/json": {"schema": {
                    "type": "array",
                    "items": {"type": "object", "properties": {"sku": {"type": "string"}}}
                }}}},
                "responses": {"201": {"description": "Created"}}
            }},
            "/orders/{id}": {"get": {
                "operationId": "getOrder",
                "summary": "One order",
                "parameters": [
                    {"name": "id", "in": "path", "required": true, "schema": {"type": "string"}},
                    {"name": "tenant-id", "in": "header", "required": true,
                     "description": "Required if the token is a JWT.", "schema": {"type": "string"}},
                    {"name": "expand", "in": "query", "required": false,
                     "schema": {"type": "array", "items": {"type": "string"}}}
                ],
                "responses": {"200": {"description": "OK"}}
            }}
        }
    }))
    .unwrap();
    WizardInput {
        spec_json: spec,
        slug: "shop".into(),
        base_url: "https://shop.example.com".into(),
        auth_header: None,
        annotations: Vec::new(),
    }
}

#[test]
fn a_header_does_not_stop_an_operation_being_a_tool() {
    // A specification marks a header required when it means "required if the
    // token is a JWT", which is no reason to leave the operation uncallable.
    let output = generate(&shop_input()).unwrap();
    let file = &output.files["get_order.md"];
    let op = crate::skill::parse(file)
        .unwrap()
        .operation
        .expect("get_order is a tool");
    // And a list of strings is a list the tool takes, not one it leaves out:
    // a pet store's search by tags answers 400 without its tags.
    assert_eq!(op.query, ["expand"], "{op:?}");
    assert_eq!(
        op.arguments_schema()["properties"]["expand"]["type"],
        "array",
        "{op:?}"
    );
    assert!(
        file.contains("which this tool does not send"),
        "the header is described, as one the tool leaves out: {file}"
    );
    assert!(file.contains("tenant-id"), "{file}");
}

#[test]
fn a_body_that_is_not_json_is_not_declared_or_forgotten() {
    // An upload's body is the file. Read as no body at all, the operation
    // became a tool that posts nothing; it is spelled out instead, saying what
    // the body is.
    let spec = serde_json::to_vec(&serde_json::json!({
        "openapi": "3.0.0",
        "info": {"title": "Test", "version": "1.0"},
        "paths": {"/pet/{petId}/uploadImage": {"post": {
            "operationId": "uploadFile",
            "summary": "Uploads an image",
            "parameters": [
                {"name": "petId", "in": "path", "required": true, "schema": {"type": "integer"}},
                {"name": "additionalMetadata", "in": "query", "schema": {"type": "string"}}
            ],
            "requestBody": {"content": {"application/octet-stream": {
                "schema": {"type": "string", "format": "binary"}
            }}},
            "responses": {"200": {"description": "OK"}}
        }}}
    }))
    .unwrap();
    let output = generate(&WizardInput {
        spec_json: spec,
        slug: "pets".into(),
        base_url: "https://pets.example.com".into(),
        auth_header: None,
        annotations: Vec::new(),
    })
    .unwrap();
    let file = &output.files["upload_file.md"];
    assert!(
        crate::skill::parse(file).unwrap().operation.is_none(),
        "{file}"
    );
    assert!(file.contains("application/octet-stream"), "{file}");
    assert!(file.contains("## The call"), "{file}");
}

#[test]
fn a_nested_body_is_a_tool_with_its_types_shared() {
    // What the flat lists could not say: a pet's category is an object, and a
    // type two operations use is written once, in types.json.
    let spec = serde_json::to_vec(&serde_json::json!({
        "openapi": "3.0.0",
        "info": {"title": "Pets", "version": "1"},
        "paths": {
            "/pet": {
                "post": {"operationId": "addPet", "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Pet"}}}}, "responses": {"200": {"description": "OK"}}},
                "put": {"operationId": "updatePet", "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Pet"}}}}, "responses": {"200": {"description": "OK"}}}
            }
        },
        "components": {"schemas": {
            "Category": {"type": "object", "properties": {"id": {"type": "integer"}, "name": {"type": "string"}}},
            "Pet": {"type": "object", "required": ["name"], "properties": {
                "id": {"type": "integer", "readOnly": true},
                "name": {"type": "string"},
                "category": {"$ref": "#/components/schemas/Category"},
                "status": {"type": "string", "enum": ["available", "sold"]}
            }}
        }}
    }))
    .unwrap();
    let output = generate(&WizardInput {
        spec_json: spec,
        slug: "pets".into(),
        base_url: "https://pets.example.com".into(),
        auth_header: None,
        annotations: Vec::new(),
    })
    .unwrap();
    let add = crate::skill::parse(&output.files["add_pet.md"])
        .unwrap()
        .operation
        .expect("add_pet is a tool");
    let args = add.arguments_schema();
    assert_eq!(
        args["properties"]["category"],
        serde_json::json!({"$ref": "#/types/Category"})
    );
    assert_eq!(
        args["properties"]["status"]["enum"],
        serde_json::json!(["available", "sold"])
    );
    assert!(
        args["properties"].get("id").is_none(),
        "readOnly is the server's"
    );
    let types: serde_json::Value = serde_json::from_str(&output.files["types.json"]).unwrap();
    assert!(types.get("Category").is_some(), "{types}");
    assert!(
        types.get("Pet").is_none(),
        "only what a tool refers to: {types}"
    );
}

#[test]
fn an_identity_is_suggested_for_a_write_and_applied_only_when_confirmed() {
    // Guessed from a create's required fields, and offered: applied unasked,
    // a wrong guess would merge writes somebody meant to be two.
    let outline = outline(&small_spec()).unwrap();
    let create = outline
        .operations
        .iter()
        .find(|o| o.name == "create_item")
        .unwrap();
    assert_eq!(create.suggested_identity, ["name"]);
    let list = outline
        .operations
        .iter()
        .find(|o| o.name == "list_items")
        .unwrap();
    assert!(list.suggested_identity.is_empty(), "a read has none");

    let unconfirmed = generate(&small_input(Vec::new())).unwrap();
    let op = crate::skill::parse(&unconfirmed.files["create_item.md"])
        .unwrap()
        .operation
        .unwrap();
    assert!(op.identity.is_empty(), "not applied unasked");

    let confirmed = generate(&small_input(vec![Annotation {
        target: Target::Operation("create_item".into()),
        kind: AnnotationKind::Identity(vec!["name".into(), "gone".into()]),
    }]))
    .unwrap();
    let op = crate::skill::parse(&confirmed.files["create_item.md"])
        .unwrap()
        .operation
        .unwrap();
    assert_eq!(
        op.identity,
        ["name"],
        "a field the specification lacks is left out"
    );
}

fn payments_spec() -> Vec<u8> {
    let json = |props: serde_json::Value, required: serde_json::Value| {
        serde_json::json!({"content": {"application/json": {"schema": {
            "type": "object", "required": required, "properties": props
        }}}})
    };
    serde_json::to_vec(&serde_json::json!({
        "openapi": "3.0.0",
        "info": {"title": "Payments", "version": "1"},
        "paths": {
            "/charges": {
                "get": {"operationId": "listCharges", "summary": "List charges", "tags": ["Charges"]},
                "post": {
                    "operationId": "createCharge", "summary": "Charge a card", "tags": ["Charges"],
                    "requestBody": json(serde_json::json!({
                        "customer_id": {"type": "string"},
                        "amount_pence": {"type": "integer", "minimum": 0},
                        "note": {"type": "string"}
                    }), serde_json::json!(["customer_id", "amount_pence"]))
                }
            },
            "/charges/{id}/refund": {
                "post": {
                    "operationId": "refundCharge", "summary": "Refund a charge", "tags": ["Charges"],
                    "requestBody": json(serde_json::json!({
                        "amount": {"type": "number", "description": "How much to give back, in USD"},
                        "reason": {"type": "string"}
                    }), serde_json::json!(["amount"]))
                }
            },
            "/transfers": {
                "post": {
                    "operationId": "createTransfer", "summary": "Move money", "tags": ["Transfers"],
                    "requestBody": json(serde_json::json!({
                        "destination": {"type": "string"},
                        "total": {"type": "integer"},
                        "currency": {"type": "string", "enum": ["EUR"]}
                    }), serde_json::json!(["destination", "total"]))
                }
            },
            "/customers": {
                "post": {
                    "operationId": "createCustomer", "summary": "Add a customer", "tags": ["Customers"],
                    "requestBody": json(serde_json::json!({
                        "name": {"type": "string"},
                        "email": {"type": "string"}
                    }), serde_json::json!(["name"]))
                }
            },
            "/customers/{id}": {
                "get": {"operationId": "getCustomer", "summary": "Fetch a customer", "tags": ["Customers"]},
                "patch": {
                    "operationId": "updateCustomer", "summary": "Rename a customer", "tags": ["Customers"],
                    "requestBody": json(serde_json::json!({"name": {"type": "string"}}), serde_json::json!([]))
                },
                "delete": {"operationId": "deleteCustomer", "summary": "Remove a customer", "tags": ["Customers"]}
            },
            "/reports": {
                "post": {
                    "operationId": "runReport", "summary": "Run a report", "tags": ["Reports"],
                    "x-risk": "high",
                    "requestBody": json(serde_json::json!({"kind": {"type": "string"}}), serde_json::json!(["kind"]))
                }
            },
            "/payouts": {
                "post": {
                    "operationId": "createPayout", "summary": "Pay out", "tags": ["Transfers"],
                    "x-risk": "low",
                    "requestBody": json(serde_json::json!({"destination": {"type": "string"}}), serde_json::json!(["destination"]))
                }
            },
            "/notices": {
                "post": {
                    "operationId": "postNotice", "summary": "Post a notice", "tags": ["Reports"],
                    "x-risk": {"level": "low", "gated": false},
                    "requestBody": json(serde_json::json!({"text": {"type": "string"}}), serde_json::json!(["text"]))
                }
            }
        }
    }))
    .unwrap()
}

fn suggestion(name: &str) -> SuggestedApproval {
    outline(&payments_spec())
        .unwrap()
        .operations
        .into_iter()
        .find(|o| o.name == name)
        .unwrap_or_else(|| panic!("no operation {name}"))
        .suggested_approval
}

#[test]
fn reads_are_low_and_ungated() {
    for name in ["list_charges", "get_customer"] {
        let s = suggestion(name);
        assert_eq!((s.risk, s.gate), (Risk::Low, false), "{name}");
        assert!(s.amount.is_none(), "{name}");
    }
}

#[test]
fn creates_and_updates_are_medium_and_ungated() {
    for name in ["create_customer", "update_customer"] {
        let s = suggestion(name);
        assert_eq!((s.risk, s.gate), (Risk::Medium, false), "{name}");
    }
}

#[test]
fn deletes_and_money_are_high_and_gated() {
    for (name, requires) in [
        ("delete_customer", "delete"),
        ("create_charge", "charge"),
        ("refund_charge", "refund"),
        ("create_transfer", "transfer"),
    ] {
        let s = suggestion(name);
        assert_eq!((s.risk, s.gate), (Risk::High, true), "{name}");
        assert_eq!(s.requires, requires, "{name}");
    }
}

#[test]
fn a_word_that_only_looks_like_a_dangerous_one_is_not_one() {
    use super::risk::is_form_of;
    assert!(is_form_of("charges", "charge"));
    assert!(is_form_of("charging", "charge"));
    assert!(is_form_of("refunded", "refund"));
    assert!(!is_form_of("sender", "send"));
    assert!(!is_form_of("payload", "pay"));
}

#[test]
fn the_specifications_own_risk_can_raise_a_guess_but_not_lower_it() {
    // Medium and ungated by name; declared high.
    let s = suggestion("run_report");
    assert_eq!((s.risk, s.gate), (Risk::High, true));
    // Medium by name; a declared low changes nothing.
    let s = suggestion("post_notice");
    assert_eq!((s.risk, s.gate), (Risk::Medium, false));
    // A payout stays a payout whatever the specification says.
    let s = suggestion("create_payout");
    assert_eq!((s.risk, s.gate), (Risk::High, true));
}

#[test]
fn an_amount_is_found_by_name_with_its_currency_and_units() {
    let charge = suggestion("create_charge").amount.unwrap();
    assert_eq!(charge.field, "amount_pence");
    assert_eq!(charge.currency.as_deref(), Some("GBP"));
    assert_eq!(charge.minor_units, Some(2));

    // A currency in the field's description, and no claim about units.
    let refund = suggestion("refund_charge").amount.unwrap();
    assert_eq!(refund.field, "amount");
    assert_eq!(refund.currency.as_deref(), Some("USD"));
    assert_eq!(refund.minor_units, None);

    // A sibling currency field with one value allowed.
    let transfer = suggestion("create_transfer").amount.unwrap();
    assert_eq!(transfer.field, "total");
    assert_eq!(transfer.currency.as_deref(), Some("EUR"));

    assert!(suggestion("create_customer").amount.is_none());
}

#[test]
fn a_rule_binds_required_fields_and_the_amount_and_compares_the_numbers() {
    let s = suggestion("create_charge");
    assert_eq!(s.binds, ["amount_pence", "customer_id"]);
    assert_eq!(s.numbers, ["amount_pence"]);
    assert_eq!(s.matches_path, "/charges");
}

#[test]
fn a_gate_stops_at_the_first_path_parameter() {
    assert_eq!(suggestion("refund_charge").matches_path, "/charges/*");
    // Nothing in the body to bind: a rule cannot be written, and says so by
    // having no binds rather than by inventing one.
    let delete = suggestion("delete_customer");
    assert_eq!(delete.matches_path, "/customers/*");
    assert!(delete.binds.is_empty());
}

fn payments_input(annotations: Vec<Annotation>) -> WizardInput {
    WizardInput {
        spec_json: payments_spec(),
        slug: "payments".into(),
        base_url: "https://api.example.com".into(),
        auth_header: None,
        annotations,
    }
}

#[test]
fn nothing_suggested_is_applied_by_generating() {
    outline(&payments_spec()).unwrap();
    let output = generate(&payments_input(Vec::new())).unwrap();
    for (path, file) in &output.files {
        assert!(
            !file.contains("approval:"),
            "{path} declares an approval nobody confirmed"
        );
    }
}

#[test]
fn a_confirmed_suggestion_is_a_rule_the_parser_reads() {
    let s = suggestion("create_charge");
    let yaml = format!(
        "approval:\n  requires: {}\n  matches: POST {}\n  binds: [{}]\n  risk: {}\n  numbers: [{}]",
        s.requires,
        s.matches_path,
        s.binds.join(", "),
        s.risk.as_str(),
        s.numbers.join(", ")
    );
    let output = generate(&payments_input(vec![Annotation {
        target: Target::Operation("create_charge".into()),
        kind: AnnotationKind::Approval(yaml),
    }]))
    .unwrap();
    let rule = crate::skill::parse(&output.files["create_charge.md"])
        .unwrap()
        .approval
        .expect("the confirmed rule");
    assert_eq!(rule.requires, "charge");
    assert_eq!(rule.numbers, ["amount_pence"]);
}

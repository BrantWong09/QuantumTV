# TVBox Fields Parsing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend admin config parser to preserve TVBox-specific fields during subscription normalization.

**Architecture:** Create a helper function `extract_tvbox_fields` that extracts TVBox-specific fields from a site JSON object and returns them as a map. Use this helper in `normalize_source_config_item()` and `normalize_api_site_object()`. In `normalize_admin_config_value()`, extract top-level `spider` field for TVBox sites format and inject it into each site that lacks a `jar` field.

**Tech Stack:** Rust, serde_json

## Global Constraints
- Maintain backward compatibility with existing config formats
- Preserve existing field insertion order (key, name, api, detail, from, disabled, is_adult, then TVBox fields)
- Default values: `searchable` = `1`, `site_type` = `1`
- `jar` field takes priority over global `spider`

---

### Task 1: Add helper function `extract_tvbox_fields`

**Files:**
- Modify: `crates/core/src/admin_config.rs:149-222` (add helper function)
- Test: `crates/core/src/admin_config.rs` (add test for helper)

**Interfaces:**
- Consumes: `&Value` (site JSON object)
- Produces: `Map<String, Value>` containing TVBox fields with defaults

- [ ] **Step 1: Write failing test for helper function**

Add test at the end of `mod tests`:

```rust
#[test]
fn extract_tvbox_fields_extracts_all_fields() {
    let input = json!({
        "searchable": 0,
        "quick_search": 1,
        "filterable": 0,
        "changeable": 1,
        "jar": "http://example.com/jar",
        "type": 3
    });
    let fields = extract_tvbox_fields(&input);
    assert_eq!(fields.get("searchable"), Some(&Value::Number(0.into())));
    assert_eq!(fields.get("quick_search"), Some(&Value::Number(1.into())));
    assert_eq!(fields.get("filterable"), Some(&Value::Number(0.into())));
    assert_eq!(fields.get("changeable"), Some(&Value::Number(1.into())));
    assert_eq!(fields.get("jar"), Some(&Value::String("http://example.com/jar".to_string())));
    assert_eq!(fields.get("site_type"), Some(&Value::Number(3.into())));
}

#[test]
fn extract_tvbox_fields_uses_defaults() {
    let input = json!({});
    let fields = extract_tvbox_fields(&input);
    assert_eq!(fields.get("searchable"), Some(&Value::Number(1.into())));
    assert_eq!(fields.get("site_type"), Some(&Value::Number(1.into())));
    assert!(fields.get("quick_search").is_none());
    assert!(fields.get("filterable").is_none());
    assert!(fields.get("changeable").is_none());
    assert!(fields.get("jar").is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p quantumsync-core admin_config::tests::extract_tvbox_fields_extracts_all_fields -- --nocapture`
Expected: FAIL with "function `extract_tvbox_fields` not found"

- [ ] **Step 3: Write minimal implementation**

Add helper function before `normalize_source_config_item`:

```rust
fn extract_tvbox_fields(item: &Value) -> Map<String, Value> {
    let mut fields = Map::new();
    
    let searchable = item.get("searchable")
        .and_then(|v| v.as_i64())
        .unwrap_or(1);
    fields.insert("searchable".to_string(), Value::Number(searchable.into()));
    
    if let Some(quick_search) = item.get("quick_search") {
        fields.insert("quick_search".to_string(), quick_search.clone());
    }
    
    if let Some(filterable) = item.get("filterable") {
        fields.insert("filterable".to_string(), filterable.clone());
    }
    
    if let Some(changeable) = item.get("changeable") {
        fields.insert("changeable".to_string(), changeable.clone());
    }
    
    if let Some(jar) = item.get("jar") {
        fields.insert("jar".to_string(), jar.clone());
    }
    
    let site_type = item.get("type")
        .and_then(|v| v.as_i64())
        .unwrap_or(1);
    fields.insert("site_type".to_string(), Value::Number(site_type.into()));
    
    fields
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p quantumsync-core admin_config::tests::extract_tvbox_fields_extracts_all_fields -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/admin_config.rs
git commit -m "feat(config): add helper to extract TVBox fields"
```

### Task 2: Integrate helper into `normalize_source_config_item`

**Files:**
- Modify: `crates/core/src/admin_config.rs:149-222` (add extraction after detail)
- Test: `crates/core/src/admin_config.rs` (add test for TVBox fields)

**Interfaces:**
- Consumes: `extract_tvbox_fields` helper
- Produces: Updated normalized JSON object with TVBox fields

- [ ] **Step 1: Write failing test for TVBox fields in normalized source**

Add test:

```rust
#[test]
fn normalize_source_config_item_extracts_tvbox_fields() {
    let input = json!({
        "name": "Test",
        "api": "http://test.com/api",
        "searchable": 0,
        "type": 3,
        "jar": "http://example.com/jar"
    });
    let result = normalize_source_config_item(&input, "custom", 0, 0).unwrap();
    let obj = result.as_object().unwrap();
    assert_eq!(obj.get("searchable"), Some(&Value::Number(0.into())));
    assert_eq!(obj.get("site_type"), Some(&Value::Number(3.into())));
    assert_eq!(obj.get("jar"), Some(&Value::String("http://example.com/jar".to_string())));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p quantumsync-core admin_config::tests::normalize_source_config_item_extracts_tvbox_fields -- --nocapture`
Expected: FAIL with assertion error

- [ ] **Step 3: Write minimal implementation**

In `normalize_source_config_item`, after line 211 (extraction of `detail`), add:

```rust
let tvbox_fields = extract_tvbox_fields(item);
for (k, v) in tvbox_fields {
    obj.insert(k, v);
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p quantumsync-core admin_config::tests::normalize_source_config_item_extracts_tvbox_fields -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/admin_config.rs
git commit -m "feat(config): extract TVBox fields in source normalization"
```

### Task 3: Integrate helper into `normalize_api_site_object`

**Files:**
- Modify: `crates/core/src/admin_config.rs:224-270` (add extraction)
- Test: `crates/core/src/admin_config.rs` (add test)

**Interfaces:**
- Consumes: `extract_tvbox_fields` helper
- Produces: Updated normalized JSON object with TVBox fields

- [ ] **Step 1: Write failing test for TVBox fields in api_site format**

Add test:

```rust
#[test]
fn normalize_api_site_object_extracts_tvbox_fields() {
    let input = json!({
        "example.com": {
            "name": "Example",
            "api": "http://example.com/api",
            "searchable": 0,
            "type": 3
        }
    });
    let result = normalize_api_site_object(input.as_object().unwrap());
    let source = result[0].as_object().unwrap();
    assert_eq!(source.get("searchable"), Some(&Value::Number(0.into())));
    assert_eq!(source.get("site_type"), Some(&Value::Number(3.into())));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p quantumsync-core admin_config::tests::normalize_api_site_object_extracts_tvbox_fields -- --nocapture`
Expected: FAIL with assertion error

- [ ] **Step 3: Write minimal implementation**

In `normalize_api_site_object`, after line 247 (extraction of `detail`), add:

```rust
let tvbox_fields = extract_tvbox_fields(value);
for (k, v) in tvbox_fields {
    obj.insert(k, v);
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p quantumsync-core admin_config::tests::normalize_api_site_object_extracts_tvbox_fields -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/admin_config.rs
git commit -m "feat(config): extract TVBox fields in api_site normalization"
```

### Task 4: Handle global `spider` field in TVBox sites format

**Files:**
- Modify: `crates/core/src/admin_config.rs:39-41` (extract spider)
- Test: `crates/core/src/admin_config.rs` (add test for spider injection)

**Interfaces:**
- Consumes: Top-level `spider` field from TVBox config
- Produces: Each site gets `spider` field if missing `jar`

- [ ] **Step 1: Write failing test for spider injection**

Add test:

```rust
#[test]
fn parse_tvbox_sites_injects_global_spider() {
    let input = json!({
        "spider": "http://global.com/jar",
        "sites": [
            {"name": "SiteA", "api": "csp_SiteA", "type": 3},
            {"name": "SiteB", "api": "csp_SiteB", "jar": "http://local.com/jar", "type": 3}
        ]
    });
    let result = parse_admin_config(&input.to_string()).unwrap();
    let sources = result.get("SourceConfig").unwrap().as_array().unwrap();
    assert_eq!(sources.len(), 2);
    let site_a = sources[0].as_object().unwrap();
    let site_b = sources[1].as_object().unwrap();
    assert_eq!(site_a.get("spider"), Some(&Value::String("http://global.com/jar".to_string())));
    assert!(site_b.get("spider").is_none());
    assert_eq!(site_b.get("jar"), Some(&Value::String("http://local.com/jar".to_string())));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p quantumsync-core admin_config::tests::parse_tvbox_sites_injects_global_spider -- --nocapture`
Expected: FAIL with assertion error

- [ ] **Step 3: Write minimal implementation**

In `normalize_admin_config_value`, modify the TVBox `sites` format handler (lines 39-41):

```rust
} else if let Some(sites) = map.get("sites").and_then(|v| v.as_array()) {
    let spider = map.get("spider").and_then(|v| v.as_str()).map(|s| s.to_string());
    let sources = normalize_source_config_array_with_spider(sites, "config", spider);
    Ok(build_config_with_sources(sources))
}
```

Add new function:

```rust
fn normalize_source_config_array_with_spider(
    items: &[Value],
    default_from: &str,
    spider: Option<String>,
) -> Vec<Value> {
    let now_ms = current_time_ms();
    items
        .iter()
        .enumerate()
        .filter_map(|(idx, item)| {
            let mut source = normalize_source_config_item(item, default_from, now_ms, idx)?;
            if let Some(ref spider_url) = spider {
                if let Some(obj) = source.as_object_mut() {
                    if !obj.contains_key("jar") {
                        obj.insert("spider".to_string(), Value::String(spider_url.clone()));
                    }
                }
            }
            Some(source)
        })
        .collect()
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p quantumsync-core admin_config::tests::parse_tvbox_sites_injects_global_spider -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/admin_config.rs
git commit -m "feat(config): inject global spider URL in TVBox sites"
```

### Task 5: Run full test suite and lint

**Files:**
- No file changes

**Interfaces:**
- N/A

- [ ] **Step 1: Run all tests**

Run: `cargo test -p quantumsync-core`
Expected: All tests pass

- [ ] **Step 2: Run clippy**

Run: `cargo clippy -p quantumsync-core -- -D warnings`
Expected: No warnings

- [ ] **Step 3: Run rustfmt**

Run: `cargo fmt --check`
Expected: No formatting issues

- [ ] **Step 4: Commit any formatting fixes if needed**

```bash
cargo fmt
git add -A
git commit -m "style: apply rustfmt"
```

### Task 6: Final verification and report

**Files:**
- Create: `D:\QuantumTV\.superpowers\sdd\task-4-report.md`

**Interfaces:**
- N/A

- [ ] **Step 1: Write implementation report**

Create report summarizing changes, tests added, and verification results.

- [ ] **Step 2: Commit report**

```bash
git add .superpowers/sdd/task-4-report.md
git commit -m "docs: add task 4 implementation report"
```
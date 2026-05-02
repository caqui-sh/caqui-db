use rusqlite::Connection;
use std::collections::HashMap;
use query_compiler::ir::{QueryNode, SelectField, WhereClause, WhereCondition};
use query_compiler::read::compile_select;

fn setup_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    
    conn.execute_batch("
        CREATE TABLE User (
            __id TEXT PRIMARY KEY,
            contents TEXT -- JSON array of polymorphic references
        );
        CREATE TABLE Post (
            __id TEXT PRIMARY KEY,
            title TEXT NOT NULL
        );
        CREATE TABLE Video (
            __id TEXT PRIMARY KEY,
            url TEXT NOT NULL
        );
        
        INSERT INTO Post (__id, title) VALUES ('p1', 'Hello World');
        INSERT INTO Video (__id, url) VALUES ('v1', 'http://example.com/video');
        
        -- Scenario A: Standard Hydration
        INSERT INTO User (__id, contents) VALUES ('u1', '[{\"type\":\"Post\",\"__id\":\"p1\"}, {\"type\":\"Video\",\"__id\":\"v1\"}]');
        
        -- Scenario B: Empty State Execution (NULL)
        INSERT INTO User (__id, contents) VALUES ('u2', NULL);
        
        -- Scenario C: Schema Evolution (Legacy/Unknown Discriminator)
        INSERT INTO User (__id, contents) VALUES ('u3', '[{\"type\":\"UnknownType\",\"__id\":\"99\"}, {\"type\":\"Post\",\"__id\":\"p1\"}]');

        -- Scenario D: Recursive Scoping
        INSERT INTO User (__id, contents) VALUES ('u4', '[{\"type\":\"User\",\"__id\":\"u5\"}]');
        INSERT INTO User (__id, contents) VALUES ('u5', '[{\"type\":\"Post\",\"__id\":\"p1\"}]');
    ").unwrap();
    
    conn
}

fn build_query() -> QueryNode {
    let post_fragment = QueryNode {
        primary_key: "__id".to_string(),
        source: query_compiler::ir::QueryIrSource::Table("Post".to_string()),
        alias: "t1".to_string(),
        order_by: vec![], selections: vec![SelectField::Scalar("title".to_string())],
        filters: None,
        limit: None,
        offset: None,
    };
    
    let video_fragment = QueryNode {
        primary_key: "__id".to_string(),
        source: query_compiler::ir::QueryIrSource::Table("Video".to_string()),
        alias: "t2".to_string(),
        order_by: vec![], selections: vec![SelectField::Scalar("url".to_string())],
        filters: None,
        limit: None,
        offset: None,
    };
    
    let mut fragments = HashMap::new();
    fragments.insert("Post".to_string(), post_fragment);
    fragments.insert("Video".to_string(), video_fragment);
    
    QueryNode {
        primary_key: "__id".to_string(),
        source: query_compiler::ir::QueryIrSource::Table("User".to_string()),
        alias: "t0".to_string(),
        order_by: vec![], selections: vec![
            SelectField::Scalar("__id".to_string()),
            SelectField::Polymorphic {
                field_name: "contents".to_string(),
                is_list: true,
                target_fragments: fragments,
            }
        ],
        filters: None,
        limit: None,
        offset: None,
    }
}

fn fetch_payload(conn: &Connection, query: &QueryNode, user_id: &str) -> String {
    let mut scoped_query = query.clone();
    scoped_query.filters = Some(WhereClause::Field("__id".to_string(), WhereCondition::Eq(user_id.to_string())));
    
    let sql = compile_select(&scoped_query, None);
    
    let mut stmt = conn.prepare(&sql).unwrap();
    let mut rows = stmt.query([]).unwrap();
    
    if let Some(row) = rows.next().unwrap() {
        row.get(0).unwrap()
    } else {
        panic!("Query returned no rows");
    }
}

#[test]
fn test_e2e_union_array_standard_hydration() {
    let conn = setup_db();
    let query = build_query();
    
    let payload = fetch_payload(&conn, &query, "u1");
    // Expected to return an array of 1 user, containing the `contents` array with hydrated objects.
    let json: serde_json::Value = serde_json::from_str(&payload).unwrap();
    let user = &json.as_array().unwrap()[0];
    
    assert_eq!(user["__id"], "u1");
    let contents = user["contents"].as_array().unwrap();
    assert_eq!(contents.len(), 2);
    assert_eq!(contents[0]["title"], "Hello World");
    assert_eq!(contents[1]["url"], "http://example.com/video");
}

#[test]
fn test_e2e_union_array_empty_state() {
    let conn = setup_db();
    let query = build_query();
    
    let payload = fetch_payload(&conn, &query, "u2");
    let json: serde_json::Value = serde_json::from_str(&payload).unwrap();
    let user = &json.as_array().unwrap()[0];
    
    assert_eq!(user["__id"], "u2");
    
    // SQLite json_each(NULL) returns zero rows, so json_group_array over an empty set
    // returns a string containing an empty array "[]", or null. We parse it:
    let contents = &user["contents"];
    
    // Ensure we don't crash and we get some valid representation of emptiness (empty array or null)
    assert!(contents.is_null() || contents.as_array().map_or(false, |a| a.is_empty()));
}

#[test]
fn test_e2e_union_array_legacy_discriminator() {
    let conn = setup_db();
    let query = build_query();
    
    let payload = fetch_payload(&conn, &query, "u3");
    let json: serde_json::Value = serde_json::from_str(&payload).unwrap();
    let user = &json.as_array().unwrap()[0];
    
    assert_eq!(user["__id"], "u3");
    let contents = user["contents"].as_array().unwrap();
    
    assert_eq!(contents.len(), 2);
    // The "UnknownType" should have triggered the ELSE NULL branch
    assert!(contents[0].is_null());
    assert_eq!(contents[1]["title"], "Hello World");
}

fn build_recursive_query() -> QueryNode {
    let post_fragment = QueryNode {
        primary_key: "__id".to_string(),
        source: query_compiler::ir::QueryIrSource::Table("Post".to_string()),
        alias: "t2".to_string(), // deep alias
        order_by: vec![], selections: vec![SelectField::Scalar("title".to_string())],
        filters: None,
        limit: None,
        offset: None,
    };
    
    let mut inner_fragments = HashMap::new();
    inner_fragments.insert("Post".to_string(), post_fragment);
    
    let user_fragment = QueryNode {
        primary_key: "__id".to_string(),
        source: query_compiler::ir::QueryIrSource::Table("User".to_string()),
        alias: "t1".to_string(), // inner alias
        order_by: vec![], selections: vec![
            SelectField::Scalar("__id".to_string()),
            SelectField::Polymorphic {
                field_name: "contents".to_string(),
                is_list: true,
                target_fragments: inner_fragments,
            }
        ],
        filters: None,
        limit: None,
        offset: None,
    };
    
    let mut outer_fragments = HashMap::new();
    outer_fragments.insert("User".to_string(), user_fragment);
    
    QueryNode {
        primary_key: "__id".to_string(),
        source: query_compiler::ir::QueryIrSource::Table("User".to_string()),
        alias: "t0".to_string(),
        order_by: vec![], selections: vec![
            SelectField::Scalar("__id".to_string()),
            SelectField::Polymorphic {
                field_name: "contents".to_string(),
                is_list: true,
                target_fragments: outer_fragments,
            }
        ],
        filters: None,
        limit: None,
        offset: None,
    }
}

#[test]
fn test_e2e_union_array_recursive_scoping() {
    let conn = setup_db();
    let query = build_recursive_query();
    
    let payload = fetch_payload(&conn, &query, "u4");
    let json: serde_json::Value = serde_json::from_str(&payload).unwrap();
    let user = &json.as_array().unwrap()[0];
    
    assert_eq!(user["__id"], "u4");
    let contents = user["contents"].as_array().unwrap();
    assert_eq!(contents.len(), 1);
    
    let inner_user = &contents[0];
    assert_eq!(inner_user["__id"], "u5");
    
    let inner_contents = inner_user["contents"].as_array().unwrap();
    assert_eq!(inner_contents.len(), 1);
    assert_eq!(inner_contents[0]["title"], "Hello World");
}
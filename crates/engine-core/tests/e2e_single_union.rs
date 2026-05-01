use rusqlite::Connection;
use std::collections::HashMap;
use query_compiler::ir::{QueryNode, SelectField, WhereClause, WhereCondition};
use query_compiler::read::compile_select;

fn setup_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    
    conn.execute_batch("
        CREATE TABLE User (
            id TEXT PRIMARY KEY,
            content_type TEXT, -- Polymorphic discriminator
            content_id TEXT    -- Polymorphic reference
        );
        CREATE TABLE Post (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL
        );
        CREATE TABLE Video (
            id TEXT PRIMARY KEY,
            url TEXT NOT NULL
        );
        
        INSERT INTO Post (id, title) VALUES ('p1', 'Hello World');
        INSERT INTO Video (id, url) VALUES ('v1', 'http://example.com/video');
        
        -- User 1 points to a Post
        INSERT INTO User (id, content_type, content_id) VALUES ('u1', 'Post', 'p1');
        
        -- User 2 points to a Video
        INSERT INTO User (id, content_type, content_id) VALUES ('u2', 'Video', 'v1');
        
        -- User 3 points to NULL (empty)
        INSERT INTO User (id, content_type, content_id) VALUES ('u3', NULL, NULL);

        -- User 4 points to an unknown discriminator
        INSERT INTO User (id, content_type, content_id) VALUES ('u4', 'UnknownType', '99');
    ").unwrap();
    
    conn
}

fn build_query() -> QueryNode {
    let post_fragment = QueryNode {
        primary_key: "id".to_string(),
        source: query_compiler::ir::QueryIrSource::Table("Post".to_string()),
        alias: "t1".to_string(),
        selections: vec![SelectField::Scalar("title".to_string())],
        filters: None,
        limit: None,
        offset: None,
    };
    
    let video_fragment = QueryNode {
        primary_key: "id".to_string(),
        source: query_compiler::ir::QueryIrSource::Table("Video".to_string()),
        alias: "t2".to_string(),
        selections: vec![SelectField::Scalar("url".to_string())],
        filters: None,
        limit: None,
        offset: None,
    };
    
    let mut fragments = HashMap::new();
    fragments.insert("Post".to_string(), post_fragment);
    fragments.insert("Video".to_string(), video_fragment);
    
    QueryNode {
        primary_key: "id".to_string(),
        source: query_compiler::ir::QueryIrSource::Table("User".to_string()),
        alias: "t0".to_string(),
        selections: vec![
            SelectField::Scalar("id".to_string()),
            SelectField::PolymorphicUnion {
                field_name: "content".to_string(),
                is_list: false,
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
    scoped_query.filters = Some(WhereClause::Field("id".to_string(), WhereCondition::Eq(user_id.to_string())));
    
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
fn test_e2e_single_union_standard_hydration() {
    let conn = setup_db();
    let query = build_query();
    
    // Test Post hydration
    let payload = fetch_payload(&conn, &query, "u1");
    let json: serde_json::Value = serde_json::from_str(&payload).unwrap();
    let user = &json.as_array().unwrap()[0];
    
    assert_eq!(user["id"], "u1");
    assert_eq!(user["content"]["title"], "Hello World");

    // Test Video hydration
    let payload2 = fetch_payload(&conn, &query, "u2");
    let json2: serde_json::Value = serde_json::from_str(&payload2).unwrap();
    let user2 = &json2.as_array().unwrap()[0];

    assert_eq!(user2["id"], "u2");
    assert_eq!(user2["content"]["url"], "http://example.com/video");
}

#[test]
fn test_e2e_single_union_empty_state() {
    let conn = setup_db();
    let query = build_query();
    
    let payload = fetch_payload(&conn, &query, "u3");
    let json: serde_json::Value = serde_json::from_str(&payload).unwrap();
    let user = &json.as_array().unwrap()[0];
    
    assert_eq!(user["id"], "u3");
    assert!(user["content"].is_null());
}

#[test]
fn test_e2e_single_union_legacy_discriminator() {
    let conn = setup_db();
    let query = build_query();
    
    let payload = fetch_payload(&conn, &query, "u4");
    let json: serde_json::Value = serde_json::from_str(&payload).unwrap();
    let user = &json.as_array().unwrap()[0];
    
    assert_eq!(user["id"], "u4");
    assert!(user["content"].is_null()); // Should gracefully fallback to NULL
}
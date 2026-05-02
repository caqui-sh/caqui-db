use schema_parser::parser::parse_schema;
use schema_parser::ast::{SchemaAst, FieldNode, AstFieldType};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::process::{Command, exit};
use rusqlite::Connection;

fn format_type(field: &FieldNode) -> String {
    let base = match &field.field_type {
        AstFieldType::Scalar(t) | AstFieldType::Relation(t) | AstFieldType::PolymorphicUnion(t) | AstFieldType::PolymorphicBase(t) => t.clone(),
        AstFieldType::ScalarArray(t) | AstFieldType::RelationArray(t) | AstFieldType::PolymorphicUnionArray(t) | AstFieldType::PolymorphicBaseArray(t) => format!("{}[]", t),
    };
    if field.is_optional {
        format!("{}?", base)
    } else {
        base
    }
}

pub fn run_diff(old_ref: &str) {
    let current_dir = std::env::current_dir().expect("Failed to get current directory");
    
    // Create temp directory for extraction
    let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
    let tmp_dir_name = format!("diff-{}", timestamp);
    let tmp_dir = current_dir.join(".caqui").join(&tmp_dir_name);
    
    if let Err(e) = fs::create_dir_all(&tmp_dir) {
        eprintln!("Failed to create temp dir: {}", e);
        exit(1);
    }
    
    // Extract historical state
    // We assume 'schema.cq' and 'app.db' are at the root
    let archive_cmd = format!("git archive {} schema.cq app.db | tar -x -C {}", old_ref, tmp_dir.display());
    let _ = Command::new("sh")
        .arg("-c")
        .arg(&archive_cmd)
        .status()
        .expect("Failed to execute git archive");
        
    // Wait for flush
    Command::new("sync").status().unwrap();
    
    // It's possible that historical commit doesn't have schema.cq or app.db
    let old_schema_path = tmp_dir.join("schema.cq");
    let old_db_path = tmp_dir.join("app.db");
    
    let old_schema_text = fs::read_to_string(&old_schema_path).unwrap_or_default();
    let new_schema_text = fs::read_to_string(current_dir.join("schema.cq")).unwrap_or_default();
    
    let old_ast = parse_schema(&old_schema_text).unwrap_or_else(|_| SchemaAst { bases: std::collections::HashMap::new(), models: HashMap::new(), unions: HashMap::new() });
    let new_ast = parse_schema(&new_schema_text).unwrap_or_else(|_| SchemaAst { bases: std::collections::HashMap::new(), models: HashMap::new(), unions: HashMap::new() });
    
    let mut diff_report: HashMap<String, ModelDiff> = HashMap::new();
    
    // Schema Diffing
    for (model_name, new_model) in &new_ast.models {
        let mut report = ModelDiff::default();
        
        if let Some(old_model) = old_ast.models.get(model_name) {
            let old_fields: HashMap<String, &FieldNode> = old_model.fields.iter().map(|f| (f.name.clone(), f)).collect();
            let new_fields: HashMap<String, &FieldNode> = new_model.fields.iter().map(|f| (f.name.clone(), f)).collect();
            
            for f in &new_model.fields {
                if let Some(old_f) = old_fields.get(&f.name) {
                    let old_type = format_type(old_f);
                    let new_type = format_type(f);
                    if old_type != new_type {
                        // Color yellow: \x1b[33m
                        report.schema_changes.push(format!("\x1b[33m  ~ Changed `{}` from {} to {}\x1b[0m", f.name, old_type, new_type));
                    }
                } else {
                    // Color green: \x1b[32m
                    report.schema_changes.push(format!("\x1b[32m  + Added field `{}` ({})\x1b[0m", f.name, format_type(f)));
                }
            }
            
            for f in &old_model.fields {
                if !new_fields.contains_key(&f.name) {
                    // Color red: \x1b[31m
                    report.schema_changes.push(format!("\x1b[31m  - Dropped field `{}`\x1b[0m", f.name));
                }
            }
        } else {
            // Color green: \x1b[32m
            report.schema_changes.push(format!("\x1b[32m  + Added Model `{}`\x1b[0m", model_name));
        }
        
        diff_report.insert(model_name.clone(), report);
    }
    
    // Data Diffing
    let new_db_path = current_dir.join("app.db");
    
    // Load custom git VFS for our connections. We assume it's already bootstrapped by `proxy_git_command`? No, proxy_git_command spawns a git sub-process.
    // Wait, proxy_git_command does not bootstrap VFS in Rust, it just delegates to git.
    // We are inside `caqui git diff`, so we must bootstrap VFS ourselves to read `file:app.db?vfs=git`.
    crate::vfs::bootstrap_custom_vfs();
    
    let new_uri = format!("file:{}?vfs=git", new_db_path.display());
    let old_uri = format!("file:{}?vfs=git", old_db_path.display());
    
    let mut db_opt = None;
    for _ in 0..5 {
        if let Ok(conn) = Connection::open_with_flags(&new_uri, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_URI) {
            let _ = conn.execute("PRAGMA cache_size=0", ());
            if conn.execute(&format!("ATTACH DATABASE '{}' AS old_db", old_uri), ()).is_ok() {
                db_opt = Some(conn);
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    
    if let Some(db) = db_opt {
        for (model_name, report) in diff_report.iter_mut() {
            let new_model = match new_ast.models.get(model_name) {
                Some(m) => m,
                None => continue,
            };
            
            let table_name = model_name; 
            
            // Find PK (always __id now)
            let pk_field = "__id".to_string();
            
            let mut table_exists_in_old = false;
            if let Ok(mut stmt) = db.prepare("SELECT 1 FROM old_db.sqlite_master WHERE type='table' AND name=?") {
                table_exists_in_old = stmt.query_row([table_name], |_| Ok(())).is_ok();
            }
            
            // 1. Added
            // Actually we want all fields in json_object.
            let mut json_obj_args = Vec::new();
            json_obj_args.push(format!("'__id'"));
            json_obj_args.push(format!("\"__id\""));
            for f in &new_model.fields {
                if let AstFieldType::Scalar(_) = f.field_type {
                    json_obj_args.push(format!("'{}'", f.name));
                    json_obj_args.push(format!("\"{}\"", f.name));
                }
            }
            let json_obj_str = if json_obj_args.is_empty() {
                format!("json_object('{}', \"{}\")", pk_field, pk_field)
            } else {
                format!("json_object({})", json_obj_args.join(", "))
            };
            
            let added_q = if table_exists_in_old {
                format!(
                    "SELECT {} FROM main.\"{}\" WHERE \"{}\" NOT IN (SELECT \"{}\" FROM old_db.\"{}\") LIMIT 50",
                    json_obj_str, table_name, pk_field, pk_field, table_name
                )
            } else {
                format!(
                    "SELECT {} FROM main.\"{}\" LIMIT 50",
                    json_obj_str, table_name
                )
            };
            
            if let Ok(mut stmt) = db.prepare(&added_q) {
                let rows = stmt.query_map([], |row| row.get::<_, String>(0));
                if let Ok(rows) = rows {
                    for r in rows.flatten() {
                        let parsed: serde_json::Value = serde_json::from_str(&r).unwrap_or(serde_json::Value::Null);
                        let id_val = parsed.get(&pk_field).map(|v| v.to_string().replace("\"", "")).unwrap_or_else(|| "N/A".to_string());
                        let mut display_str = r.clone();
                        if display_str.len() > 60 {
                            display_str = display_str[..60].to_string();
                        }
                        report.added.push(format!("  \x1b[32m+ Inserted\x1b[0m (__id: {}): {}...", id_val, display_str));                    }
                }
            }
            
            // 2. Deleted
            if table_exists_in_old {
                let deleted_q = format!(
                    "SELECT {} FROM old_db.\"{}\" WHERE \"{}\" NOT IN (SELECT \"{}\" FROM main.\"{}\") LIMIT 50",
                    json_obj_str, table_name, pk_field, pk_field, table_name
                );
                if let Ok(mut stmt) = db.prepare(&deleted_q) {
                    let rows = stmt.query_map([], |row| row.get::<_, String>(0));
                    if let Ok(rows) = rows {
                        for r in rows.flatten() {
                            let parsed: serde_json::Value = serde_json::from_str(&r).unwrap_or(serde_json::Value::Null);
                            let id_val = parsed.get(&pk_field).map(|v| v.to_string().replace("\"", "")).unwrap_or_else(|| "N/A".to_string());
                            let mut display_str = r.clone();
                            if display_str.len() > 60 {
                                display_str = display_str[..60].to_string();
                            }
                            report.deleted.push(format!("  \x1b[31m- Deleted \x1b[0m (__id: {}): {}...", id_val, display_str));
                        }
                    }
                }
                
                // 3. Modified
                if let Some(old_model) = old_ast.models.get(model_name) {
                    let old_fields_set: HashSet<String> = old_model.fields.iter().map(|f| f.name.clone()).collect();
                    let mut scalar_fields = Vec::new();
                    for f in &new_model.fields {
                        if let AstFieldType::Scalar(_) = f.field_type {
                            if f.name != pk_field && old_fields_set.contains(&f.name) {
                                scalar_fields.push(&f.name);
                            }
                        }
                    }
                    
                    if !scalar_fields.is_empty() {
                        let mut wheres = Vec::new();
                        
                        for f in &scalar_fields {
                            wheres.push(format!("new_t.\"{}\" IS NOT old_t.\"{}\"", f, f));
                        }
                        
                        // We must execute dynamically since we don't know the types
                        // We can use sqlite's json_object again for the modified rows!
                        let mut json_mod_args = vec![format!("'__pk'"), format!("new_t.\"{}\"", pk_field)];
                        for f in &scalar_fields {
                            json_mod_args.push(format!("'old_{}'", f));
                            json_mod_args.push(format!("old_t.\"{}\"", f));
                            json_mod_args.push(format!("'new_{}'", f));
                            json_mod_args.push(format!("new_t.\"{}\"", f));
                        }
                        
                        let mod_json_q = format!(
                            "SELECT json_object({}) FROM main.\"{}\" AS new_t JOIN old_db.\"{}\" AS old_t ON new_t.\"{}\" = old_t.\"{}\" WHERE {} LIMIT 50",
                            json_mod_args.join(", "), table_name, table_name, pk_field, pk_field, wheres.join(" OR ")
                        );
                        
                        if let Ok(mut stmt) = db.prepare(&mod_json_q) {
                            let rows = stmt.query_map([], |row| row.get::<_, String>(0));
                            if let Ok(rows) = rows {
                                for r in rows.flatten() {
                                    let parsed: serde_json::Value = serde_json::from_str(&r).unwrap_or(serde_json::Value::Null);
                                    if let Some(obj) = parsed.as_object() {
                                        let id_val = obj.get("__pk").map(|v| v.to_string().replace("\"", "")).unwrap_or_else(|| "N/A".to_string());
                                        let mut mod_lines = Vec::new();
                                        
                                        for f in &scalar_fields {
                                            let old_key = format!("old_{}", f);
                                            let new_key = format!("new_{}", f);
                                            let old_val = obj.get(&old_key).unwrap_or(&serde_json::Value::Null);
                                            let new_val = obj.get(&new_key).unwrap_or(&serde_json::Value::Null);
                                            
                                            if old_val != new_val {
                                                // Format string values cleanly without quotes to match JS String(val)
                                                let old_str = if let Some(s) = old_val.as_str() { s.to_string() } else { old_val.to_string() };
                                                let new_str = if let Some(s) = new_val.as_str() { s.to_string() } else { new_val.to_string() };
                                                
                                                mod_lines.push(format!("      ↳ {}: \x1b[31m{}\x1b[0m -> \x1b[32m{}\x1b[0m", f, old_str, new_str));
                                            }
                                        }
                                        
                                        if !mod_lines.is_empty() {
                                            report.modified.push(format!("  \x1b[33m~ Modified\x1b[0m (__id: {}):\n{}", id_val, mod_lines.join("\n")));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    
    // Output
    let mut has_changes = false;
    
    // Sort to ensure deterministic output
    let mut sorted_models: Vec<_> = diff_report.keys().collect();
    sorted_models.sort();
    
    for model_name in sorted_models {
        let diff = diff_report.get(model_name).unwrap();
        
        if diff.schema_changes.is_empty() && diff.added.is_empty() && diff.deleted.is_empty() && diff.modified.is_empty() {
            continue;
        }
        
        has_changes = true;
        
        println!("\n📦 \x1b[1m\x1b[34mModel: {}\x1b[0m", model_name);
        println!("\x1b[2m────────────────────────────────────────────────────────────\x1b[0m");
        
        if !diff.schema_changes.is_empty() {
            println!("\x1b[1m[Schema Changes]\x1b[0m");
            for line in &diff.schema_changes {
                println!("{}", line);
            }
            println!();
        }
        
        if !diff.added.is_empty() || !diff.deleted.is_empty() || !diff.modified.is_empty() {
            println!("\x1b[1m[Record Changes]\x1b[0m");
            
            for row in &diff.added {
                println!("{}", row);
            }
            for row in &diff.deleted {
                println!("{}", row);
            }
            for mod_entry in &diff.modified {
                println!("{}", mod_entry);
            }
        }
    }
    
    let _ = fs::remove_dir_all(&tmp_dir);
    
    if !has_changes {
        println!("No differences found.");
        exit(0);
    }
    
    exit(1);
}

#[derive(Default)]
struct ModelDiff {
    schema_changes: Vec<String>,
    added: Vec<String>,
    deleted: Vec<String>,
    modified: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_parser::ast::{FieldNode, AstFieldType};

    #[test]
    fn test_format_type() {
        // Required Scalar
        let f1 = FieldNode {
            name: "id".to_string(),
            field_type: AstFieldType::Scalar("Int".to_string()),
            is_optional: false,
            attributes: vec![],
        };
        assert_eq!(format_type(&f1), "Int");

        // Optional Scalar
        let f2 = FieldNode {
            name: "bio".to_string(),
            field_type: AstFieldType::Scalar("String".to_string()),
            is_optional: true,
            attributes: vec![],
        };
        assert_eq!(format_type(&f2), "String?");

        // Required Array
        let f3 = FieldNode {
            name: "tags".to_string(),
            field_type: AstFieldType::ScalarArray("String".to_string()),
            is_optional: false,
            attributes: vec![],
        };
        assert_eq!(format_type(&f3), "String[]");

        // Optional Array (though unusual, grammar supports it)
        let f4 = FieldNode {
            name: "notes".to_string(),
            field_type: AstFieldType::ScalarArray("String".to_string()),
            is_optional: true,
            attributes: vec![],
        };
        assert_eq!(format_type(&f4), "String[]?");

        // Polymorphic Union Array
        let f5 = FieldNode {
            name: "results".to_string(),
            field_type: AstFieldType::PolymorphicUnionArray("SearchResult".to_string()),
            is_optional: false,
            attributes: vec![],
        };
        assert_eq!(format_type(&f5), "SearchResult[]");
    }
}

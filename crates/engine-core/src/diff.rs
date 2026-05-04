use schema_parser::parser::parse_schema;
use schema_parser::ast::{SchemaAst, FieldNode, AstFieldType, FieldAttribute, ModelAttribute, DefaultFunc};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::process::{Command, exit};
use rusqlite::Connection;

#[derive(Default)]
pub struct SchemaDiffReport {
    pub models: HashMap<String, ModelDiff>,
    pub enums: HashMap<String, TopLevelDiff>,
    pub unions: HashMap<String, TopLevelDiff>,
    pub bases: HashMap<String, TopLevelDiff>,
}

#[derive(Default)]
pub struct TopLevelDiff {
    pub schema_changes: Vec<String>,
}

#[derive(Default)]
pub struct ModelDiff {
    pub schema_changes: Vec<String>,
    pub added: Vec<String>,
    pub deleted: Vec<String>,
    pub modified: Vec<String>,
}

fn format_field_attribute(attr: &FieldAttribute) -> Option<String> {
    match attr {
        FieldAttribute::Id => Some("@id".to_string()),
        FieldAttribute::Unique => Some("@unique".to_string()),
        FieldAttribute::Track => Some("@track".to_string()),
        FieldAttribute::Relation { name, on_delete, .. } => {
            let mut parts = Vec::new();
            if let Some(n) = name { parts.push(format!("name: \"{}\"", n)); }
            if let Some(od) = on_delete { parts.push(format!("onDelete: {}", od)); }
            if parts.is_empty() {
                Some("@relation".to_string())
            } else {
                Some(format!("@relation({})", parts.join(", ")))
            }
        }
        _ => None, // Internal attributes are ignored
    }
}

fn format_model_attribute(attr: &ModelAttribute) -> Option<String> {
    match attr {
        ModelAttribute::Id(func) => {
            match func {
                DefaultFunc::Uuid => Some("@@id(uuid)".to_string()),
                DefaultFunc::Cuid => Some("@@id(cuid)".to_string()),
                DefaultFunc::AutoIncrement => Some("@@id(autoincrement)".to_string()),
                _ => Some("@@id".to_string()),
            }
        },
        ModelAttribute::Track => Some("@@track".to_string()),
        ModelAttribute::FullText(fields) => Some(format!("@@fulltext([{}])", fields.join(", "))),
    }
}

fn format_type(field: &FieldNode) -> String {
    let base = match &field.field_type {
        AstFieldType::Scalar(t) | AstFieldType::Relation(t) | AstFieldType::PolymorphicUnion(t) | AstFieldType::PolymorphicBase(t) | AstFieldType::Enum(t) => t.clone(),
        AstFieldType::ScalarArray(t) | AstFieldType::RelationArray(t) | AstFieldType::PolymorphicUnionArray(t) | AstFieldType::PolymorphicBaseArray(t) | AstFieldType::EnumArray(t) => format!("{}[]", t),
    };
    if field.is_optional {
        format!("{}?", base)
    } else {
        base
    }
}

pub fn compute_schema_diff(old_ast: &SchemaAst, new_ast: &SchemaAst) -> SchemaDiffReport {
    let mut report = SchemaDiffReport::default();

    // 1. Models
    for (model_name, new_model) in &new_ast.models {
        let mut model_diff = ModelDiff::default();
        
        if let Some(old_model) = old_ast.models.get(model_name) {
            let old_fields: HashMap<String, &FieldNode> = old_model.fields.iter().map(|f| (f.name.clone(), f)).collect();
            let new_fields: HashMap<String, &FieldNode> = new_model.fields.iter().map(|f| (f.name.clone(), f)).collect();
            
            let mut old_block_attrs: HashSet<String> = HashSet::new();
            for attr in &old_model.block_attributes {
                if let Some(s) = format_model_attribute(attr) { old_block_attrs.insert(s); }
            }
            let mut new_block_attrs: HashSet<String> = HashSet::new();
            for attr in &new_model.block_attributes {
                if let Some(s) = format_model_attribute(attr) { new_block_attrs.insert(s); }
            }
            for attr in &new_block_attrs {
                if !old_block_attrs.contains(attr) {
                    model_diff.schema_changes.push(format!("\x1b[32m  + Added Model Attribute: {}\x1b[0m", attr));
                }
            }
            for attr in &old_block_attrs {
                if !new_block_attrs.contains(attr) {
                    model_diff.schema_changes.push(format!("\x1b[31m  - Dropped Model Attribute: {}\x1b[0m", attr));
                }
            }

            for f in &new_model.fields {
                if let Some(old_f) = old_fields.get(&f.name) {
                    let old_type = format_type(old_f);
                    let new_type = format_type(f);
                    if old_type != new_type {
                        // Color yellow: \x1b[33m
                        model_diff.schema_changes.push(format!("\x1b[33m  ~ Changed `{}` from {} to {}\x1b[0m", f.name, old_type, new_type));
                    }
                    
                    let mut old_attrs: HashSet<String> = HashSet::new();
                    for attr in &old_f.attributes {
                        if let Some(s) = format_field_attribute(attr) { old_attrs.insert(s); }
                    }
                    let mut new_attrs: HashSet<String> = HashSet::new();
                    for attr in &f.attributes {
                        if let Some(s) = format_field_attribute(attr) { new_attrs.insert(s); }
                    }
                    for attr in &new_attrs {
                        if !old_attrs.contains(attr) {
                            model_diff.schema_changes.push(format!("\x1b[33m  ~ Changed `{}` ({}): Added {}\x1b[0m", f.name, new_type, attr));
                        }
                    }
                    for attr in &old_attrs {
                        if !new_attrs.contains(attr) {
                            model_diff.schema_changes.push(format!("\x1b[33m  ~ Changed `{}` ({}): Dropped {}\x1b[0m", f.name, new_type, attr));
                        }
                    }
                } else {
                    // Color green: \x1b[32m
                    model_diff.schema_changes.push(format!("\x1b[32m  + Added field `{}` ({})\x1b[0m", f.name, format_type(f)));
                }
            }
            
            for f in &old_model.fields {
                if !new_fields.contains_key(&f.name) {
                    // Color red: \x1b[31m
                    model_diff.schema_changes.push(format!("\x1b[31m  - Dropped field `{}`\x1b[0m", f.name));
                }
            }
        } else {
            // Color green: \x1b[32m
            model_diff.schema_changes.push(format!("\x1b[32m  + Added Model `{}`\x1b[0m", model_name));
        }
        
        report.models.insert(model_name.clone(), model_diff);
    }
    for old_model_name in old_ast.models.keys() {
        if !new_ast.models.contains_key(old_model_name) {
            let mut model_diff = ModelDiff::default();
            model_diff.schema_changes.push(format!("\x1b[31m  - Dropped Model `{}`\x1b[0m", old_model_name));
            report.models.insert(old_model_name.clone(), model_diff);
        }
    }

    // 2. Enums
    for (enum_name, new_variants) in &new_ast.enums {
        let mut enum_diff = TopLevelDiff::default();
        if let Some(old_variants) = old_ast.enums.get(enum_name) {
            let old_set: HashSet<&String> = old_variants.iter().collect();
            let new_set: HashSet<&String> = new_variants.iter().collect();

            for v in new_variants {
                if !old_set.contains(v) {
                    enum_diff.schema_changes.push(format!("\x1b[32m  + Added variant `{}`\x1b[0m", v));
                }
            }
            for v in old_variants {
                if !new_set.contains(v) {
                    enum_diff.schema_changes.push(format!("\x1b[31m  - Dropped variant `{}`\x1b[0m", v));
                }
            }
        } else {
            enum_diff.schema_changes.push(format!("\x1b[32m  + Added Enum `{}`\x1b[0m", enum_name));
        }
        report.enums.insert(enum_name.clone(), enum_diff);
    }
    for old_enum_name in old_ast.enums.keys() {
        if !new_ast.enums.contains_key(old_enum_name) {
            let mut enum_diff = TopLevelDiff::default();
            enum_diff.schema_changes.push(format!("\x1b[31m  - Dropped Enum `{}`\x1b[0m", old_enum_name));
            report.enums.insert(old_enum_name.clone(), enum_diff);
        }
    }

    // 3. Unions
    for (union_name, new_variants) in &new_ast.unions {
        let mut union_diff = TopLevelDiff::default();
        if let Some(old_variants) = old_ast.unions.get(union_name) {
            let old_set: HashSet<&String> = old_variants.iter().collect();
            let new_set: HashSet<&String> = new_variants.iter().collect();

            for v in new_variants {
                if !old_set.contains(v) {
                    union_diff.schema_changes.push(format!("\x1b[32m  + Added variant `{}`\x1b[0m", v));
                }
            }
            for v in old_variants {
                if !new_set.contains(v) {
                    union_diff.schema_changes.push(format!("\x1b[31m  - Dropped variant `{}`\x1b[0m", v));
                }
            }
        } else {
            union_diff.schema_changes.push(format!("\x1b[32m  + Added Union `{}`\x1b[0m", union_name));
        }
        report.unions.insert(union_name.clone(), union_diff);
    }
    for old_union_name in old_ast.unions.keys() {
        if !new_ast.unions.contains_key(old_union_name) {
            let mut union_diff = TopLevelDiff::default();
            union_diff.schema_changes.push(format!("\x1b[31m  - Dropped Union `{}`\x1b[0m", old_union_name));
            report.unions.insert(old_union_name.clone(), union_diff);
        }
    }

    // 4. Bases
    for (base_name, new_base) in &new_ast.bases {
        let mut base_diff = TopLevelDiff::default();
        if let Some(old_base) = old_ast.bases.get(base_name) {
            let old_fields: HashMap<String, &FieldNode> = old_base.fields.iter().map(|f| (f.name.clone(), f)).collect();
            let new_fields: HashMap<String, &FieldNode> = new_base.fields.iter().map(|f| (f.name.clone(), f)).collect();

            for f in &new_base.fields {
                if let Some(old_f) = old_fields.get(&f.name) {
                    let old_type = format_type(old_f);
                    let new_type = format_type(f);
                    if old_type != new_type {
                        base_diff.schema_changes.push(format!("\x1b[33m  ~ Changed `{}` from {} to {}\x1b[0m", f.name, old_type, new_type));
                    }
                    
                    let mut old_attrs: HashSet<String> = HashSet::new();
                    for attr in &old_f.attributes {
                        if let Some(s) = format_field_attribute(attr) { old_attrs.insert(s); }
                    }
                    let mut new_attrs: HashSet<String> = HashSet::new();
                    for attr in &f.attributes {
                        if let Some(s) = format_field_attribute(attr) { new_attrs.insert(s); }
                    }
                    for attr in &new_attrs {
                        if !old_attrs.contains(attr) {
                            base_diff.schema_changes.push(format!("\x1b[33m  ~ Changed `{}` ({}): Added {}\x1b[0m", f.name, new_type, attr));
                        }
                    }
                    for attr in &old_attrs {
                        if !new_attrs.contains(attr) {
                            base_diff.schema_changes.push(format!("\x1b[33m  ~ Changed `{}` ({}): Dropped {}\x1b[0m", f.name, new_type, attr));
                        }
                    }
                } else {
                    base_diff.schema_changes.push(format!("\x1b[32m  + Added field `{}` ({})\x1b[0m", f.name, format_type(f)));
                }
            }
            
            for f in &old_base.fields {
                if !new_fields.contains_key(&f.name) {
                    base_diff.schema_changes.push(format!("\x1b[31m  - Dropped field `{}`\x1b[0m", f.name));
                }
            }
        } else {
            base_diff.schema_changes.push(format!("\x1b[32m  + Added Base `{}`\x1b[0m", base_name));
        }
        report.bases.insert(base_name.clone(), base_diff);
    }
    for old_base_name in old_ast.bases.keys() {
        if !new_ast.bases.contains_key(old_base_name) {
            let mut base_diff = TopLevelDiff::default();
            base_diff.schema_changes.push(format!("\x1b[31m  - Dropped Base `{}`\x1b[0m", old_base_name));
            report.bases.insert(old_base_name.clone(), base_diff);
        }
    }

    report
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
    
    let old_ast = parse_schema(&old_schema_text).unwrap_or_else(|_| SchemaAst { bases: std::collections::HashMap::new(), models: HashMap::new(), unions: HashMap::new(), enums: HashMap::new() });
    let new_ast = parse_schema(&new_schema_text).unwrap_or_else(|_| SchemaAst { bases: std::collections::HashMap::new(), models: HashMap::new(), unions: HashMap::new(), enums: HashMap::new() });
    
    let mut diff_report = compute_schema_diff(&old_ast, &new_ast);
    
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
        for (model_name, report) in diff_report.models.iter_mut() {
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
                if let Some(_old_model) = old_ast.models.get(model_name) {
                    let mut cols = Vec::new();
                    if let Ok(mut stmt) = db.prepare(&format!("PRAGMA main.table_info(\"{}\")", table_name)) {
                        let rows = stmt.query_map([], |row| row.get::<_, String>(1));
                        if let Ok(rows) = rows {
                            for r in rows.flatten() {
                                if r != pk_field && !r.starts_with("__") {
                                    // check if it exists in old_db too
                                    let mut exists_in_old = false;
                                    if let Ok(mut old_stmt) = db.prepare(&format!("PRAGMA old_db.table_info(\"{}\")", table_name)) {
                                        if let Ok(old_rows) = old_stmt.query_map([], |orow| orow.get::<_, String>(1)) {
                                            exists_in_old = old_rows.flatten().any(|or| or == r);
                                        }
                                    }
                                    if exists_in_old {
                                        cols.push(r);
                                    }
                                }
                            }
                        }
                    }
                    
                    if !cols.is_empty() {
                        let mut wheres = Vec::new();
                        
                        for f in &cols {
                            wheres.push(format!("new_t.\"{}\" IS NOT old_t.\"{}\"", f, f));
                        }
                        
                        // We must execute dynamically since we don't know the types
                        // We can use sqlite's json_object again for the modified rows!
                        let mut json_mod_args = vec![format!("'__pk'"), format!("new_t.\"{}\"", pk_field)];
                        for f in &cols {
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
                                        
                                        for f in &cols {
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

    // Enums
    let mut sorted_enums: Vec<_> = diff_report.enums.keys().collect();
    sorted_enums.sort();
    for enum_name in sorted_enums {
        let diff = diff_report.enums.get(enum_name).unwrap();
        if diff.schema_changes.is_empty() { continue; }
        has_changes = true;
        println!("\n📦 \x1b[1m\x1b[35mEnum: {}\x1b[0m", enum_name); // Magenta
        println!("\x1b[2m────────────────────────────────────────────────────────────\x1b[0m");
        for line in &diff.schema_changes {
            println!("{}", line);
        }
    }

    // Unions
    let mut sorted_unions: Vec<_> = diff_report.unions.keys().collect();
    sorted_unions.sort();
    for union_name in sorted_unions {
        let diff = diff_report.unions.get(union_name).unwrap();
        if diff.schema_changes.is_empty() { continue; }
        has_changes = true;
        println!("\n📦 \x1b[1m\x1b[36mUnion: {}\x1b[0m", union_name); // Cyan
        println!("\x1b[2m────────────────────────────────────────────────────────────\x1b[0m");
        for line in &diff.schema_changes {
            println!("{}", line);
        }
    }

    // Bases
    let mut sorted_bases: Vec<_> = diff_report.bases.keys().collect();
    sorted_bases.sort();
    for base_name in sorted_bases {
        let diff = diff_report.bases.get(base_name).unwrap();
        if diff.schema_changes.is_empty() { continue; }
        has_changes = true;
        println!("\n📦 \x1b[1m\x1b[33mBase: {}\x1b[0m", base_name); // Yellow
        println!("\x1b[2m────────────────────────────────────────────────────────────\x1b[0m");
        for line in &diff.schema_changes {
            println!("{}", line);
        }
    }
    
    // Sort to ensure deterministic output
    let mut sorted_models: Vec<_> = diff_report.models.keys().collect();
    sorted_models.sort();
    
    for model_name in sorted_models {
        let diff = diff_report.models.get(model_name).unwrap();
        
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

#[cfg(test)]
mod tests {
    use super::*;
    use schema_parser::ast::{FieldNode, AstFieldType, BaseNode, ModelNode};

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

        // Enum
        let f6 = FieldNode {
            name: "role".to_string(),
            field_type: AstFieldType::Enum("Role".to_string()),
            is_optional: false,
            attributes: vec![],
        };
        assert_eq!(format_type(&f6), "Role");

        // Enum Array
        let f7 = FieldNode {
            name: "roles".to_string(),
            field_type: AstFieldType::EnumArray("Role".to_string()),
            is_optional: true,
            attributes: vec![],
        };
        assert_eq!(format_type(&f7), "Role[]?");
    }

    #[test]
    fn test_compute_schema_diff_models() {
        let mut old_ast = SchemaAst::default();
        let mut new_ast = SchemaAst::default();

        let mut old_model = ModelNode { name: "User".to_string(), ..Default::default() };
        old_model.fields.push(FieldNode { name: "id".to_string(), field_type: AstFieldType::Scalar("Int".to_string()), is_optional: false, attributes: vec![] });
        old_model.fields.push(FieldNode { name: "name".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![] });
        old_ast.models.insert("User".to_string(), old_model);

        let mut new_model = ModelNode { name: "User".to_string(), ..Default::default() };
        new_model.fields.push(FieldNode { name: "id".to_string(), field_type: AstFieldType::Scalar("Int".to_string()), is_optional: false, attributes: vec![] });
        new_model.fields.push(FieldNode { name: "name".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: true, attributes: vec![] }); // changed optional
        new_model.fields.push(FieldNode { name: "role".to_string(), field_type: AstFieldType::Enum("Role".to_string()), is_optional: false, attributes: vec![] }); // added
        new_ast.models.insert("User".to_string(), new_model);

        new_ast.models.insert("Post".to_string(), ModelNode { name: "Post".to_string(), ..Default::default() });

        let report = compute_schema_diff(&old_ast, &new_ast);

        assert_eq!(report.models.len(), 2);
        
        let user_diff = report.models.get("User").unwrap();
        assert_eq!(user_diff.schema_changes.len(), 2);
        assert!(user_diff.schema_changes.iter().any(|c| c.contains("Changed `name` from String to String?")));
        assert!(user_diff.schema_changes.iter().any(|c| c.contains("Added field `role` (Role)")));

        let post_diff = report.models.get("Post").unwrap();
        assert_eq!(post_diff.schema_changes.len(), 1);
        assert!(post_diff.schema_changes[0].contains("Added Model `Post`"));
    }

    #[test]
    fn test_compute_schema_diff_enums() {
        let mut old_ast = SchemaAst::default();
        let mut new_ast = SchemaAst::default();

        old_ast.enums.insert("Role".to_string(), vec!["ADMIN".to_string(), "USER".to_string()]);
        old_ast.enums.insert("Status".to_string(), vec!["ACTIVE".to_string()]);

        new_ast.enums.insert("Role".to_string(), vec!["ADMIN".to_string(), "MODERATOR".to_string()]);
        new_ast.enums.insert("Priority".to_string(), vec!["HIGH".to_string()]);

        let report = compute_schema_diff(&old_ast, &new_ast);

        assert_eq!(report.enums.len(), 3); // Role, Status (dropped), Priority (added)
        
        let role_diff = report.enums.get("Role").unwrap();
        assert_eq!(role_diff.schema_changes.len(), 2);
        assert!(role_diff.schema_changes.iter().any(|c| c.contains("Dropped variant `USER`")));
        assert!(role_diff.schema_changes.iter().any(|c| c.contains("Added variant `MODERATOR`")));

        let status_diff = report.enums.get("Status").unwrap();
        assert!(status_diff.schema_changes[0].contains("Dropped Enum `Status`"));

        let priority_diff = report.enums.get("Priority").unwrap();
        assert!(priority_diff.schema_changes[0].contains("Added Enum `Priority`"));
    }

    #[test]
    fn test_compute_schema_diff_unions() {
        let mut old_ast = SchemaAst::default();
        let mut new_ast = SchemaAst::default();

        old_ast.unions.insert("Media".to_string(), vec!["Image".to_string(), "Video".to_string()]);
        new_ast.unions.insert("Media".to_string(), vec!["Image".to_string(), "Audio".to_string()]);

        let report = compute_schema_diff(&old_ast, &new_ast);
        let diff = report.unions.get("Media").unwrap();
        assert_eq!(diff.schema_changes.len(), 2);
        assert!(diff.schema_changes.iter().any(|c| c.contains("Dropped variant `Video`")));
        assert!(diff.schema_changes.iter().any(|c| c.contains("Added variant `Audio`")));
    }

    #[test]
    fn test_compute_schema_diff_bases() {
        let mut old_ast = SchemaAst::default();
        let mut new_ast = SchemaAst::default();

        let mut old_base = BaseNode { name: "Timestamped".to_string(), ..Default::default() };
        old_base.fields.push(FieldNode { name: "createdAt".to_string(), field_type: AstFieldType::Scalar("DateTime".to_string()), is_optional: false, attributes: vec![] });
        old_ast.bases.insert("Timestamped".to_string(), old_base);

        let mut new_base = BaseNode { name: "Timestamped".to_string(), ..Default::default() };
        new_base.fields.push(FieldNode { name: "createdAt".to_string(), field_type: AstFieldType::Scalar("DateTime".to_string()), is_optional: false, attributes: vec![] });
        new_base.fields.push(FieldNode { name: "updatedAt".to_string(), field_type: AstFieldType::Scalar("DateTime".to_string()), is_optional: true, attributes: vec![] });
        new_ast.bases.insert("Timestamped".to_string(), new_base);

        let report = compute_schema_diff(&old_ast, &new_ast);
        let diff = report.bases.get("Timestamped").unwrap();
        assert_eq!(diff.schema_changes.len(), 1);
        assert!(diff.schema_changes[0].contains("Added field `updatedAt` (DateTime?)"));
    }
}
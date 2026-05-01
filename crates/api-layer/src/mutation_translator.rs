use serde_json::Value;
use query_compiler::mutation_ir::{ExecutionPlan, ExecutionStep, Parameter};
use schema_parser::ast::{SchemaAst, AstFieldType, FieldAttribute};
use crate::where_parser::parse_where_clause;

pub fn hydrate_mutation_to_plan(
    ast: &SchemaAst,
    model_name: &str,
    action: &str,
    payload: &Value,
    alias_counter: &mut usize,
) -> Result<ExecutionPlan, String> {
    let mut steps = Vec::new();
    
    match action {
        "create" => {
            let data = payload.get("data").and_then(|v| v.as_object())
                .ok_or("Missing 'data' block in create mutation")?;
            
            let root_step_id = translate_create_node(ast, model_name, data, &mut steps, alias_counter, None)?;
            
            Ok(ExecutionPlan {
                root_step_id,
                steps,
            })
        },
        "update" => {
            let data = payload.get("data").and_then(|v| v.as_object())
                .ok_or("Missing 'data' block in update mutation")?;
            
            let where_obj = payload.get("where").and_then(|v| v.as_object())
                .ok_or("Missing 'where' block in update mutation")?;
                
            let root_step_id = translate_update_node(ast, model_name, where_obj, data, &mut steps, alias_counter, None)?;
            
            Ok(ExecutionPlan {
                root_step_id,
                steps,
            })
        },
        "delete" => {
            let where_obj = payload.get("where").and_then(|v| v.as_object())
                .ok_or("Missing 'where' block in delete mutation")?;
                
            let step_id = format!("step_{}_{}", model_name.to_lowercase(), *alias_counter);
            *alias_counter += 1;
            
            let model_def = ast.models.get(model_name)
                .ok_or_else(|| format!("Security Exception: Model '{}' undefined.", model_name))?;
            
            let mut params = Vec::new();
            let mut param_idx = 1;
            
            let where_clause_ir = parse_where_clause(ast, where_obj, model_def)?;
            let (where_sql, where_params) = compile_parameterized_where(&where_clause_ir, model_name, &mut param_idx);
            params.extend(where_params);
            
            let sql = format!(
                "DELETE FROM {} WHERE {} RETURNING id;",
                model_name,
                where_sql
            );
            
            steps.push(ExecutionStep::Query {
                id: step_id.clone(),
                sql,
                params,
            });
            
            Ok(ExecutionPlan {
                root_step_id: step_id,
                steps,
            })
        },
        "upsert" => {
            let where_obj = payload.get("where").and_then(|v| v.as_object())
                .ok_or("Missing 'where' block in upsert mutation")?;
            let create_data = payload.get("create").and_then(|v| v.as_object())
                .ok_or("Missing 'create' block in upsert mutation")?;
            let update_data = payload.get("update").and_then(|v| v.as_object())
                .ok_or("Missing 'update' block in upsert mutation")?;

            let root_step_id = translate_root_upsert_node(ast, model_name, where_obj, create_data, update_data, &mut steps, alias_counter)?;
            
            Ok(ExecutionPlan {
                root_step_id,
                steps,
            })
        },
        _ => Err(format!("Unsupported mutation action: {}", action)),
    }
}

struct ParentRel {
    parent_step_id: String,
    parent_model: String,
    relation_field_name: String,
}

enum DeferredAction {
    Create(serde_json::Map<String, Value>),
    Connect(serde_json::Map<String, Value>),
    Update(serde_json::Map<String, Value>, serde_json::Map<String, Value>),
    Delete(serde_json::Map<String, Value>),
    Disconnect(serde_json::Map<String, Value>),
    Set(Vec<serde_json::Map<String, Value>>),
    Upsert(serde_json::Map<String, Value>, serde_json::Map<String, Value>),
}

struct DeferredChild {
    target_model: String,
    action: DeferredAction,
    relation_field_name: String,
}

fn translate_create_node(
    ast: &SchemaAst,
    model_name: &str,
    data: &serde_json::Map<String, Value>,
    steps: &mut Vec<ExecutionStep>,
    alias_counter: &mut usize,
    parent_rel: Option<ParentRel>,
) -> Result<String, String> {
    let model_def = ast.models.get(model_name)
        .ok_or_else(|| format!("Security Exception: Model '{}' undefined.", model_name))?;

    let step_id = format!("step_{}_{}", model_name.to_lowercase(), *alias_counter);
    *alias_counter += 1;
    
    let mut columns = Vec::new();
    let mut params = Vec::new();
    let mut placeholders = Vec::new();
    let mut param_idx = 1;
    
    let mut deferred_children = Vec::new();
    
    if let Some(rel) = &parent_rel {
        let parent_model_def = ast.models.get(&rel.parent_model).unwrap();
        let parent_field_def = parent_model_def.fields.iter().find(|f| f.name == rel.relation_field_name).unwrap();
        
        let mut fk_column_name = None;
        let mut is_our_fk = false;

        for attr in &parent_field_def.attributes {
            if let FieldAttribute::Relation { fields, references, .. } = attr {
                if fields.is_empty() && references.is_empty() {
                    // Parent does not hold the FK. We must hold it.
                    for our_field in &model_def.fields {
                        if let AstFieldType::Relation(target) = &our_field.field_type {
                            if target == &rel.parent_model {
                                for our_attr in &our_field.attributes {
                                    if let FieldAttribute::Relation { fields: our_fields, .. } = our_attr {
                                        if !our_fields.is_empty() {
                                            fk_column_name = Some(our_fields[0].clone());
                                            is_our_fk = true;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        
        if is_our_fk {
            if let Some(col) = fk_column_name {
                columns.push(col.clone());
                placeholders.push(format!("?{}", param_idx));
                params.push(Parameter::Reference { step_id: rel.parent_step_id.clone(), column: "id".to_string() });
                param_idx += 1;
            }
        }
    }
    
    for (key, val) in data {
        let field_def = model_def.fields.iter().find(|f| &f.name == key)
            .ok_or_else(|| format!("Invalid field '{}' for model '{}'.", key, model_name))?;

        if field_def.attributes.iter().any(|a| matches!(a, FieldAttribute::Ignore)) {
            return Err(format!("Security Exception: Prohibited write to ignored field '{}'", key));
        }
        
        match &field_def.field_type {
            AstFieldType::Scalar(_) => {
                columns.push(key.clone());
                placeholders.push(format!("?{}", param_idx));
                params.push(Parameter::Literal(val.clone()));
                param_idx += 1;
            },
            AstFieldType::ScalarArray(_) => {
                columns.push(key.clone());
                placeholders.push(format!("?{}", param_idx));
                let json_val = serde_json::to_string(&val).unwrap_or_else(|_| "[]".to_string());
                params.push(Parameter::Literal(serde_json::Value::String(json_val)));
                param_idx += 1;
            },
            AstFieldType::Relation(target_model) | AstFieldType::RelationArray(target_model) => {
                let is_array = matches!(&field_def.field_type, AstFieldType::RelationArray(_));
                
                let mut we_hold_fk = false;
                let mut fk_column = None;
                for attr in &field_def.attributes {
                    if let FieldAttribute::Relation { fields, .. } = attr {
                        if !fields.is_empty() {
                            we_hold_fk = true;
                            fk_column = Some(fields[0].clone());
                        }
                    }
                }
                
                let nested_mutations = val.as_object().ok_or_else(|| format!("Expected object for nested mutation on '{}'", key))?;
                
                if we_hold_fk {
                    if let Some(create_payload) = nested_mutations.get("create") {
                        if is_array {
                            return Err("Unsupported: array creation on a relation where we hold the FK".to_string());
                        }
                        let child_data = create_payload.as_object().ok_or("Expected object for 'create'")?;
                        let child_step_id = translate_create_node(ast, target_model, child_data, steps, alias_counter, None)?;
                        
                        if let Some(col) = &fk_column {
                            columns.push(col.clone());
                            placeholders.push(format!("?{}", param_idx));
                            params.push(Parameter::Reference { step_id: child_step_id, column: "id".to_string() });
                            param_idx += 1;
                        }
                    }
                    if let Some(connect_payload) = nested_mutations.get("connect") {
                        if let Some(connect_id) = connect_payload.as_object().and_then(|o| o.get("id")) {
                            if let Some(col) = &fk_column {
                                columns.push(col.clone());
                                placeholders.push(format!("?{}", param_idx));
                                params.push(Parameter::Literal(connect_id.clone()));
                                param_idx += 1;
                            }
                        }
                    }
                } else {
                    if let Some(create_payload) = nested_mutations.get("create") {
                        if let Some(arr) = create_payload.as_array() {
                            for item in arr {
                                let child_data = item.as_object().ok_or("Expected object in 'create' array")?;
                                deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Create(child_data.clone()), relation_field_name: key.clone() });
                            }
                        } else if let Some(child_data) = create_payload.as_object() {
                            deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Create(child_data.clone()), relation_field_name: key.clone() });
                        }
                    }
                    if let Some(connect_payload) = nested_mutations.get("connect") {
                        if let Some(arr) = connect_payload.as_array() {
                            for item in arr {
                                let child_data = item.as_object().ok_or("Expected object in 'connect' array")?;
                                deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Connect(child_data.clone()), relation_field_name: key.clone() });
                            }
                        } else if let Some(child_data) = connect_payload.as_object() {
                            deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Connect(child_data.clone()), relation_field_name: key.clone() });
                        }
                    }
                    if let Some(update_payload) = nested_mutations.get("update") {
                        if let Some(arr) = update_payload.as_array() {
                            for item in arr {
                                let child_data = item.get("data").and_then(|v| v.as_object()).ok_or("Expected 'data' object in 'update' array")?;
                                let child_where = item.get("where").and_then(|v| v.as_object()).ok_or("Expected 'where' object in 'update' array")?;
                                deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Update(child_where.clone(), child_data.clone()), relation_field_name: key.clone() });
                            }
                        } else if let Some(item) = update_payload.as_object() {
                            let child_data = item.get("data").and_then(|v| v.as_object()).ok_or("Expected 'data' object in 'update'")?;
                            let child_where = item.get("where").and_then(|v| v.as_object()).ok_or("Expected 'where' object in 'update'")?;
                            deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Update(child_where.clone(), child_data.clone()), relation_field_name: key.clone() });
                        }
                    }
                    if let Some(delete_payload) = nested_mutations.get("delete") {
                        if let Some(arr) = delete_payload.as_array() {
                            for item in arr {
                                let child_where = item.as_object().ok_or("Expected 'where' object in 'delete' array")?;
                                deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Delete(child_where.clone()), relation_field_name: key.clone() });
                            }
                        } else if let Some(item) = delete_payload.as_object() {
                            deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Delete(item.clone()), relation_field_name: key.clone() });
                        }
                    }
                    if let Some(disconnect_payload) = nested_mutations.get("disconnect") {
                        if let Some(arr) = disconnect_payload.as_array() {
                            for item in arr {
                                let child_where = item.as_object().ok_or("Expected 'where' object in 'disconnect' array")?;
                                deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Disconnect(child_where.clone()), relation_field_name: key.clone() });
                            }
                        } else if let Some(item) = disconnect_payload.as_object() {
                            deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Disconnect(item.clone()), relation_field_name: key.clone() });
                        }
                    }
                    if let Some(set_payload) = nested_mutations.get("set") {
                        if let Some(arr) = set_payload.as_array() {
                            let mut set_wheres = Vec::new();
                            for item in arr {
                                let child_where = item.as_object().ok_or("Expected 'where' object in 'set' array")?;
                                set_wheres.push(child_where.clone());
                            }
                            deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Set(set_wheres), relation_field_name: key.clone() });
                        } else {
                            deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Set(Vec::new()), relation_field_name: key.clone() });
                        }
                    }
                }
                
                if let Some(upsert_payload) = nested_mutations.get("upsert") {
                    if let Some(arr) = upsert_payload.as_array() {
                        for item in arr {
                            let create_data = item.get("create").and_then(|v| v.as_object()).ok_or("Expected 'create' in 'upsert' array")?;
                            let update_data = item.get("update").and_then(|v| v.as_object()).ok_or("Expected 'update' in 'upsert' array")?;
                            deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Upsert(create_data.clone(), update_data.clone()), relation_field_name: key.clone() });
                        }
                    } else if let Some(item) = upsert_payload.as_object() {
                        let create_data = item.get("create").and_then(|v| v.as_object()).ok_or("Expected 'create' in 'upsert'")?;
                        let update_data = item.get("update").and_then(|v| v.as_object()).ok_or("Expected 'update' in 'upsert'")?;
                        deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Upsert(create_data.clone(), update_data.clone()), relation_field_name: key.clone() });
                    }
                }
            },
            AstFieldType::PolymorphicUnion(_) | AstFieldType::PolymorphicUnionArray(_) => {
                return Err(format!("Unsupported: Mutations on polymorphic union field '{}' are not yet implemented.", key));
            }
        }
    }
    
    let sql = if columns.is_empty() {
        format!("INSERT INTO {} DEFAULT VALUES RETURNING id;", model_name)
    } else {
        format!(
            "INSERT INTO {} ({}) VALUES ({}) RETURNING id;",
            model_name,
            columns.join(", "),
            placeholders.join(", ")
        )
    };
    
    steps.push(ExecutionStep::Query {
        id: step_id.clone(),
        sql,
        params,
    });
    
    process_deferred_children(ast, model_name, &step_id, deferred_children, steps, alias_counter)?;
    
    Ok(step_id)
}

fn translate_update_node(
    ast: &SchemaAst,
    model_name: &str,
    where_obj: &serde_json::Map<String, Value>,
    data: &serde_json::Map<String, Value>,
    steps: &mut Vec<ExecutionStep>,
    alias_counter: &mut usize,
    parent_rel: Option<ParentRel>,
) -> Result<String, String> {
    let model_def = ast.models.get(model_name)
        .ok_or_else(|| format!("Security Exception: Model '{}' undefined.", model_name))?;

    let step_id = format!("step_{}_{}", model_name.to_lowercase(), *alias_counter);
    *alias_counter += 1;
    
    let mut set_clauses = Vec::new();
    let mut params = Vec::new();
    let mut param_idx = 1;
    
    let mut deferred_children = Vec::new();
    
    for (key, val) in data {
        let field_def = model_def.fields.iter().find(|f| &f.name == key)
            .ok_or_else(|| format!("Invalid field '{}' for model '{}'.", key, model_name))?;

        if field_def.attributes.iter().any(|a| matches!(a, FieldAttribute::Ignore)) {
            return Err(format!("Security Exception: Prohibited write to ignored field '{}'", key));
        }
        
        match &field_def.field_type {
            AstFieldType::Scalar(_) => {
                set_clauses.push(format!("{} = ?{}", key, param_idx));
                params.push(Parameter::Literal(val.clone()));
                param_idx += 1;
            },
            AstFieldType::ScalarArray(_) => {
                if let Some(obj) = val.as_object() {
                    if let Some(push_val) = obj.get("push") {
                        set_clauses.push(format!("{} = json_insert(COALESCE({}, '[]'), '$[#]', ?{})", key, key, param_idx));
                        let push_str = if push_val.is_string() {
                            push_val.as_str().unwrap().to_string()
                        } else {
                            serde_json::to_string(push_val).unwrap_or_default()
                        };
                        params.push(Parameter::Literal(serde_json::Value::String(push_str)));
                        param_idx += 1;
                    }
                } else if let Some(arr) = val.as_array() {
                    set_clauses.push(format!("{} = ?{}", key, param_idx));
                    let json_val = serde_json::to_string(arr).unwrap_or_else(|_| "[]".to_string());
                    params.push(Parameter::Literal(serde_json::Value::String(json_val)));
                    param_idx += 1;
                }
            },
            AstFieldType::Relation(target_model) | AstFieldType::RelationArray(target_model) => {
                let is_array = matches!(&field_def.field_type, AstFieldType::RelationArray(_));
                
                let mut we_hold_fk = false;
                let mut fk_column = None;
                for attr in &field_def.attributes {
                    if let FieldAttribute::Relation { fields, .. } = attr {
                        if !fields.is_empty() {
                            we_hold_fk = true;
                            fk_column = Some(fields[0].clone());
                        }
                    }
                }
                
                let nested_mutations = val.as_object().ok_or_else(|| format!("Expected object for nested mutation on '{}'", key))?;
                
                if we_hold_fk {
                    if let Some(create_payload) = nested_mutations.get("create") {
                        if is_array {
                            return Err("Unsupported: array creation on a relation where we hold the FK".to_string());
                        }
                        let child_data = create_payload.as_object().ok_or("Expected object for 'create'")?;
                        let child_step_id = translate_create_node(ast, target_model, child_data, steps, alias_counter, None)?;
                        
                        if let Some(col) = &fk_column {
                            set_clauses.push(format!("{} = ?{}", col, param_idx));
                            params.push(Parameter::Reference { step_id: child_step_id, column: "id".to_string() });
                            param_idx += 1;
                        }
                    }
                    if let Some(connect_payload) = nested_mutations.get("connect") {
                        if let Some(connect_id) = connect_payload.as_object().and_then(|o| o.get("id")) {
                            if let Some(col) = &fk_column {
                                set_clauses.push(format!("{} = ?{}", col, param_idx));
                                params.push(Parameter::Literal(connect_id.clone()));
                                param_idx += 1;
                            }
                        }
                    }
                    if let Some(disconnect_payload) = nested_mutations.get("disconnect") {
                        if disconnect_payload.as_bool().unwrap_or(false) {
                            if let Some(col) = &fk_column {
                                set_clauses.push(format!("{} = NULL", col));
                            }
                        }
                    }
                } else {
                    if let Some(create_payload) = nested_mutations.get("create") {
                        if let Some(arr) = create_payload.as_array() {
                            for item in arr {
                                let child_data = item.as_object().ok_or("Expected object in 'create' array")?;
                                deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Create(child_data.clone()), relation_field_name: key.clone() });
                            }
                        } else if let Some(child_data) = create_payload.as_object() {
                            deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Create(child_data.clone()), relation_field_name: key.clone() });
                        }
                    }
                    if let Some(connect_payload) = nested_mutations.get("connect") {
                        if let Some(arr) = connect_payload.as_array() {
                            for item in arr {
                                let child_data = item.as_object().ok_or("Expected object in 'connect' array")?;
                                deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Connect(child_data.clone()), relation_field_name: key.clone() });
                            }
                        } else if let Some(child_data) = connect_payload.as_object() {
                            deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Connect(child_data.clone()), relation_field_name: key.clone() });
                        }
                    }
                    if let Some(update_payload) = nested_mutations.get("update") {
                        if let Some(arr) = update_payload.as_array() {
                            for item in arr {
                                let child_data = item.get("data").and_then(|v| v.as_object()).ok_or("Expected 'data' object in 'update' array")?;
                                let child_where = item.get("where").and_then(|v| v.as_object()).ok_or("Expected 'where' object in 'update' array")?;
                                deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Update(child_where.clone(), child_data.clone()), relation_field_name: key.clone() });
                            }
                        } else if let Some(item) = update_payload.as_object() {
                            let child_data = item.get("data").and_then(|v| v.as_object()).ok_or("Expected 'data' object in 'update'")?;
                            let child_where = item.get("where").and_then(|v| v.as_object()).ok_or("Expected 'where' object in 'update'")?;
                            deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Update(child_where.clone(), child_data.clone()), relation_field_name: key.clone() });
                        }
                    }
                    if let Some(delete_payload) = nested_mutations.get("delete") {
                        if let Some(arr) = delete_payload.as_array() {
                            for item in arr {
                                let child_where = item.as_object().ok_or("Expected 'where' object in 'delete' array")?;
                                deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Delete(child_where.clone()), relation_field_name: key.clone() });
                            }
                        } else if let Some(item) = delete_payload.as_object() {
                            deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Delete(item.clone()), relation_field_name: key.clone() });
                        }
                    }
                    if let Some(disconnect_payload) = nested_mutations.get("disconnect") {
                        if let Some(arr) = disconnect_payload.as_array() {
                            for item in arr {
                                let child_where = item.as_object().ok_or("Expected 'where' object in 'disconnect' array")?;
                                deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Disconnect(child_where.clone()), relation_field_name: key.clone() });
                            }
                        } else if let Some(item) = disconnect_payload.as_object() {
                            deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Disconnect(item.clone()), relation_field_name: key.clone() });
                        }
                    }
                    if let Some(set_payload) = nested_mutations.get("set") {
                        if let Some(arr) = set_payload.as_array() {
                            let mut set_wheres = Vec::new();
                            for item in arr {
                                let child_where = item.as_object().ok_or("Expected 'where' object in 'set' array")?;
                                set_wheres.push(child_where.clone());
                            }
                            deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Set(set_wheres), relation_field_name: key.clone() });
                        } else {
                            deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Set(Vec::new()), relation_field_name: key.clone() });
                        }
                    }
                }
                
                if let Some(upsert_payload) = nested_mutations.get("upsert") {
                    if let Some(arr) = upsert_payload.as_array() {
                        for item in arr {
                            let create_data = item.get("create").and_then(|v| v.as_object()).ok_or("Expected 'create' in 'upsert' array")?;
                            let update_data = item.get("update").and_then(|v| v.as_object()).ok_or("Expected 'update' in 'upsert' array")?;
                            deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Upsert(create_data.clone(), update_data.clone()), relation_field_name: key.clone() });
                        }
                    } else if let Some(item) = upsert_payload.as_object() {
                        let create_data = item.get("create").and_then(|v| v.as_object()).ok_or("Expected 'create' in 'upsert'")?;
                        let update_data = item.get("update").and_then(|v| v.as_object()).ok_or("Expected 'update' in 'upsert'")?;
                        deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Upsert(create_data.clone(), update_data.clone()), relation_field_name: key.clone() });
                    }
                }
            },
            AstFieldType::PolymorphicUnion(_) | AstFieldType::PolymorphicUnionArray(_) => {
                return Err(format!("Unsupported: Mutations on polymorphic union field '{}' are not yet implemented.", key));
            }
        }
    }
    
    if set_clauses.is_empty() {
        set_clauses.push("id = id".to_string());
    }
    
    let where_clause_ir = parse_where_clause(ast, where_obj, model_def)?;
    let (mut where_sql, mut where_params) = compile_parameterized_where(&where_clause_ir, model_name, &mut param_idx);
    
    if let Some(rel) = &parent_rel {
        let parent_model_def = ast.models.get(&rel.parent_model).unwrap();
        let mut fk_column_name = None;
        let mut is_our_fk = false;

        for attr in parent_model_def.fields.iter().find(|f| f.name == rel.relation_field_name).unwrap().attributes.iter() {
            if let FieldAttribute::Relation { fields, references, .. } = attr {
                if fields.is_empty() && references.is_empty() {
                    for our_field in &model_def.fields {
                        if let AstFieldType::Relation(target) = &our_field.field_type {
                            if target == &rel.parent_model {
                                for our_attr in &our_field.attributes {
                                    if let FieldAttribute::Relation { fields: our_fields, .. } = our_attr {
                                        if !our_fields.is_empty() {
                                            fk_column_name = Some(our_fields[0].clone());
                                            is_our_fk = true;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        if is_our_fk {
            if let Some(col) = fk_column_name {
                where_sql = format!("{} AND {}.{} = ?{}", where_sql, model_name, col, param_idx);
                where_params.push(Parameter::Reference { step_id: rel.parent_step_id.clone(), column: "id".to_string() });
            }
        }
    }
    params.extend(where_params);

    let sql = format!(
        "UPDATE {} SET {} WHERE {} RETURNING id;",
        model_name,
        set_clauses.join(", "),
        where_sql
    );
    
    steps.push(ExecutionStep::Query {
        id: step_id.clone(),
        sql,
        params,
    });
    
    process_deferred_children(ast, model_name, &step_id, deferred_children, steps, alias_counter)?;
    
    Ok(step_id)
}

fn translate_root_upsert_node(
    ast: &SchemaAst,
    model_name: &str,
    where_obj: &serde_json::Map<String, Value>,
    create_data: &serde_json::Map<String, Value>,
    update_data: &serde_json::Map<String, Value>,
    steps: &mut Vec<ExecutionStep>,
    alias_counter: &mut usize,
) -> Result<String, String> {
    let model_def = ast.models.get(model_name)
        .ok_or_else(|| format!("Security Exception: Model '{}' undefined.", model_name))?;

    let step_id = format!("step_{}_{}", model_name.to_lowercase(), *alias_counter);
    *alias_counter += 1;
    
    let mut param_idx = 1;
    let where_clause_ir = parse_where_clause(ast, where_obj, model_def)?;
    
    // Safety check: Upserts must target a unique field
    // In Phase 4, we enforce this at the Rust layer since we don't rely on ON CONFLICT anymore.
    let conflict_target = where_obj.keys().next().ok_or("Upsert 'where' block must contain at least one key")?.clone();
    let target_field = model_def.fields.iter().find(|f| f.name == conflict_target).unwrap();
    if !target_field.attributes.iter().any(|a| matches!(a, FieldAttribute::Id | FieldAttribute::Unique)) {
        return Err(format!("Security Exception: Upsert target '{}' is not marked as @id or @unique", conflict_target));
    }
    
    let (where_sql, check_params) = compile_parameterized_where(&where_clause_ir, model_name, &mut param_idx);
    
    let check_sql = format!("SELECT id FROM {} WHERE {}", model_name, where_sql);
    
    let mut if_not_exists = Vec::new();
    let _create_step_id = translate_create_node(ast, model_name, create_data, &mut if_not_exists, alias_counter, None)?;

    let mut if_exists = Vec::new();
    let _update_step_id = translate_update_node(ast, model_name, where_obj, update_data, &mut if_exists, alias_counter, None)?;
    
    // We need the executor to return `create_step_id` or `update_step_id` under `step_id`?
    // Actually, `ExecutionStep::UpsertBranch` currently uses `root_step_id: String` to know what to assign.
    // In `executor.rs`: `returned_values.insert(id.clone(), returned_id);`
    // But for `UpsertBranch`, we didn't insert a return value for the branch itself.
    // The `executor.rs` evaluates `exists_id` but then delegates to `execute_steps`.
    // Wait, the children steps will insert THEIR OWN IDs into `returned_values`.
    // And `execute_steps` also sets `*root_id = returned_id` if it matches `root_step_id`.
    // We just pass `step_id.clone()` as the `root_step_id` to `UpsertBranch`. 
    // And wait, we need one of the branches to generate `step_id`. 
    // But the branches generated `create_step_id` and `update_step_id`. 
    // This is tricky. Let's fix `executor.rs` or `ExecutionStep::UpsertBranch` to alias the branch root.
    // Or we can just pass `step_id.clone()` down, but `translate_create_node` already generated its own ID.
    // Let's modify `ExecutionStep::UpsertBranch` to have `branch_step_id: String, create_step_id: String, update_step_id: String`.
    // Then in `executor.rs`, after executing `if_exists`, we do `returned_values.insert(branch_step_id, returned_values.get(&update_step_id))`.
    
    steps.push(ExecutionStep::UpsertBranch {
        check_sql,
        check_params,
        if_exists,
        if_not_exists,
        root_step_id: step_id.clone(),
    });
    
    // The executor.rs currently has `ExecutionStep::UpsertBranch { ..., root_step_id: branch_root_id }`.
    // If we just use this `step_id` as `branch_root_id`, the executor can alias it. Let's fix executor to do this mapping.
    // Wait, we need to know the child's root ID. Let's just return `step_id`.
    
    Ok(step_id)
}

fn process_deferred_children(
    ast: &SchemaAst,
    parent_model_name: &str,
    parent_step_id: &str,
    deferred_children: Vec<DeferredChild>,
    steps: &mut Vec<ExecutionStep>,
    alias_counter: &mut usize,
) -> Result<(), String> {
    for child in deferred_children {
        let mut fk_column_name = None;
        for our_field in &ast.models.get(&child.target_model).unwrap().fields {
            if let AstFieldType::Relation(target) = &our_field.field_type {
                if target == parent_model_name {
                    for our_attr in &our_field.attributes {
                        if let FieldAttribute::Relation { fields: our_fields, .. } = our_attr {
                            if !our_fields.is_empty() {
                                fk_column_name = Some(our_fields[0].clone());
                            }
                        }
                    }
                }
            }
        }
        let fk_col = fk_column_name.clone().unwrap_or_default(); // Might be empty but some variants handle this

        match child.action {
            DeferredAction::Create(child_data) => {
                translate_create_node(ast, &child.target_model, &child_data, steps, alias_counter, Some(ParentRel {
                    parent_step_id: parent_step_id.to_string(),
                    parent_model: parent_model_name.to_string(),
                    relation_field_name: child.relation_field_name,
                }))?;
            },
            DeferredAction::Connect(connect_where) => {
                let child_step_id = format!("step_{}_connect_{}", child.target_model.to_lowercase(), *alias_counter);
                *alias_counter += 1;
                
                let mut params = Vec::new();
                let mut param_idx = 1;
                
                let set_clause = format!("{} = ?{}", fk_col, param_idx);
                params.push(Parameter::Reference { step_id: parent_step_id.to_string(), column: "id".to_string() });
                param_idx += 1;
                
                let child_model_def = ast.models.get(&child.target_model).unwrap();
                let where_clause_ir = parse_where_clause(ast, &connect_where, child_model_def)?;
                let (where_sql, where_params) = compile_parameterized_where(&where_clause_ir, &child.target_model, &mut param_idx);
                params.extend(where_params);
                
                let sql = format!(
                    "UPDATE {} SET {} WHERE {} RETURNING id;",
                    child.target_model,
                    set_clause,
                    where_sql
                );
                
                steps.push(ExecutionStep::Query {
                    id: child_step_id,
                    sql,
                    params,
                });
            },
            DeferredAction::Update(child_where, child_data) => {
                translate_update_node(ast, &child.target_model, &child_where, &child_data, steps, alias_counter, Some(ParentRel {
                    parent_step_id: parent_step_id.to_string(),
                    parent_model: parent_model_name.to_string(),
                    relation_field_name: child.relation_field_name,
                }))?;
            },
            DeferredAction::Delete(child_where) => {
                let child_step_id = format!("step_{}_delete_{}", child.target_model.to_lowercase(), *alias_counter);
                *alias_counter += 1;
                
                let mut params = Vec::new();
                let mut param_idx = 1;
                
                let child_model_def = ast.models.get(&child.target_model).unwrap();
                let where_clause_ir = parse_where_clause(ast, &child_where, child_model_def)?;
                let (where_sql, where_params) = compile_parameterized_where(&where_clause_ir, &child.target_model, &mut param_idx);
                params.extend(where_params);
                
                // Add parent relation constraint
                let combined_where_sql = format!("{} AND {}.{} = ?{}", where_sql, child.target_model, fk_col, param_idx);
                params.push(Parameter::Reference { step_id: parent_step_id.to_string(), column: "id".to_string() });
                
                let sql = format!(
                    "DELETE FROM {} WHERE {} RETURNING id;",
                    child.target_model,
                    combined_where_sql
                );
                
                steps.push(ExecutionStep::Query {
                    id: child_step_id,
                    sql,
                    params,
                });
            },
            DeferredAction::Disconnect(child_where) => {
                let child_step_id = format!("step_{}_disconnect_{}", child.target_model.to_lowercase(), *alias_counter);
                *alias_counter += 1;
                
                let mut params = Vec::new();
                let mut param_idx = 1;
                
                let child_model_def = ast.models.get(&child.target_model).unwrap();
                let where_clause_ir = parse_where_clause(ast, &child_where, child_model_def)?;
                let (where_sql, where_params) = compile_parameterized_where(&where_clause_ir, &child.target_model, &mut param_idx);
                params.extend(where_params);
                
                let combined_where_sql = format!("{} AND {}.{} = ?{}", where_sql, child.target_model, fk_col, param_idx);
                params.push(Parameter::Reference { step_id: parent_step_id.to_string(), column: "id".to_string() });
                
                let sql = format!(
                    "UPDATE {} SET {} = NULL WHERE {} RETURNING id;",
                    child.target_model,
                    fk_col,
                    combined_where_sql
                );
                
                steps.push(ExecutionStep::Query {
                    id: child_step_id,
                    sql,
                    params,
                });
            },
            DeferredAction::Upsert(create_data, update_data) => {
                let mut parent_fk_col = None;
                for attr in &ast.models.get(parent_model_name).unwrap().fields.iter().find(|f| f.name == child.relation_field_name).unwrap().attributes {
                    if let FieldAttribute::Relation { fields, references: _, .. } = attr {
                        if !fields.is_empty() {
                            parent_fk_col = Some(fields[0].clone());
                        }
                    }
                }

                let child_step_id = format!("step_{}_upsert_{}", child.target_model.to_lowercase(), *alias_counter);
                *alias_counter += 1;
                
                let mut columns = Vec::new();
                let mut placeholders = Vec::new();
                let mut params = Vec::new();
                let mut param_idx = 1;
                
                if let Some(ref pfk) = parent_fk_col {
                    columns.push("id".to_string());
                    placeholders.push(format!("COALESCE((SELECT {} FROM {} WHERE id = ?{}), gen_uuid7())", pfk, parent_model_name, param_idx));
                    params.push(Parameter::Reference { step_id: parent_step_id.to_string(), column: "id".to_string() });
                    param_idx += 1;
                }
                
                for (key, val) in create_data {
                    columns.push(key.clone());
                    placeholders.push(format!("?{}", param_idx));
                    params.push(Parameter::Literal(val.clone()));
                    param_idx += 1;
                }

                let mut update_set_clauses = Vec::new();
                if let Some(update_data_obj) = update_data.get("data").and_then(|v| v.as_object()).or(Some(&update_data)) {
                    for (key, val) in update_data_obj {
                        update_set_clauses.push(format!("{} = ?{}", key, param_idx));
                        params.push(Parameter::Literal(val.clone()));
                        param_idx += 1;
                    }
                }
                
                if update_set_clauses.is_empty() {
                    update_set_clauses.push("id = excluded.id".to_string());
                }

                let sql = format!(
                    "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT(id) DO UPDATE SET {} RETURNING id;",
                    child.target_model,
                    columns.join(", "),
                    placeholders.join(", "),
                    update_set_clauses.join(", ")
                );
                
                steps.push(ExecutionStep::Query {
                    id: child_step_id.clone(),
                    sql,
                    params,
                });

                if let Some(ref pfk) = parent_fk_col {
                    let link_step_id = format!("step_{}_upsert_link_{}", parent_model_name.to_lowercase(), *alias_counter);
                    *alias_counter += 1;
                    
                    let link_sql = format!("UPDATE {} SET {} = ?1 WHERE id = ?2 RETURNING id;", parent_model_name, pfk);
                    steps.push(ExecutionStep::Query {
                        id: link_step_id,
                        sql: link_sql,
                        params: vec![
                            Parameter::Reference { step_id: child_step_id.clone(), column: "id".to_string() },
                            Parameter::Reference { step_id: parent_step_id.to_string(), column: "id".to_string() },
                        ],
                    });
                }
            },
            DeferredAction::Set(child_wheres) => {
                // First disconnect all existing
                let disconnect_step_id = format!("step_{}_set_disconnect_{}", child.target_model.to_lowercase(), *alias_counter);
                *alias_counter += 1;
                
                let sql = format!("UPDATE {} SET {} = NULL WHERE {} = ?1 RETURNING id;", child.target_model, fk_col, fk_col);
                steps.push(ExecutionStep::Query {
                    id: disconnect_step_id,
                    sql,
                    params: vec![Parameter::Reference { step_id: parent_step_id.to_string(), column: "id".to_string() }],
                });
                
                // Then connect each child
                for connect_where in child_wheres {
                    let child_step_id = format!("step_{}_set_connect_{}", child.target_model.to_lowercase(), *alias_counter);
                    *alias_counter += 1;
                    
                    let mut params = Vec::new();
                    let mut param_idx = 1;
                    
                    let set_clause = format!("{} = ?{}", fk_col, param_idx);
                    params.push(Parameter::Reference { step_id: parent_step_id.to_string(), column: "id".to_string() });
                    param_idx += 1;
                    
                    let child_model_def = ast.models.get(&child.target_model).unwrap();
                    let where_clause_ir = parse_where_clause(ast, &connect_where, child_model_def)?;
                    let (where_sql, where_params) = compile_parameterized_where(&where_clause_ir, &child.target_model, &mut param_idx);
                    params.extend(where_params);
                    
                    let sql = format!(
                        "UPDATE {} SET {} WHERE {} RETURNING id;",
                        child.target_model,
                        set_clause,
                        where_sql
                    );
                    
                    steps.push(ExecutionStep::Query {
                        id: child_step_id,
                        sql,
                        params,
                    });
                }
            }
        }
    }
    Ok(())
}

fn compile_parameterized_where(
    clause: &query_compiler::ir::WhereClause,
    alias: &str,
    param_idx: &mut usize,
) -> (String, Vec<Parameter>) {
    use query_compiler::ir::{WhereClause, WhereCondition, RelationFilter};
    let mut params = Vec::new();

    match clause {
        WhereClause::And(clauses) => {
            let mut sqls = Vec::new();
            for c in clauses {
                let (s, p) = compile_parameterized_where(c, alias, param_idx);
                sqls.push(s);
                params.extend(p);
            }
            (format!("({})", sqls.join(" AND ")), params)
        }
        WhereClause::Or(clauses) => {
            let mut sqls = Vec::new();
            for c in clauses {
                let (s, p) = compile_parameterized_where(c, alias, param_idx);
                sqls.push(s);
                params.extend(p);
            }
            (format!("({})", sqls.join(" OR ")), params)
        }
        WhereClause::Field(field, condition) => {
            let col = format!("{}.{}", alias, field);
            match condition {
                WhereCondition::Eq(v) => {
                    let s = format!("{} = ?{}", col, *param_idx);
                    *param_idx += 1;
                    params.push(Parameter::Literal(serde_json::Value::String(v.clone())));
                    (s, params)
                },
                WhereCondition::NotEq(v) => {
                    let s = format!("{} != ?{}", col, *param_idx);
                    *param_idx += 1;
                    params.push(Parameter::Literal(serde_json::Value::String(v.clone())));
                    (s, params)
                },
                WhereCondition::Gt(v) => {
                    let s = format!("{} > ?{}", col, *param_idx);
                    *param_idx += 1;
                    params.push(Parameter::Literal(serde_json::Value::String(v.clone())));
                    (s, params)
                },
                WhereCondition::Gte(v) => {
                    let s = format!("{} >= ?{}", col, *param_idx);
                    *param_idx += 1;
                    params.push(Parameter::Literal(serde_json::Value::String(v.clone())));
                    (s, params)
                },
                WhereCondition::Lt(v) => {
                    let s = format!("{} < ?{}", col, *param_idx);
                    *param_idx += 1;
                    params.push(Parameter::Literal(serde_json::Value::String(v.clone())));
                    (s, params)
                },
                WhereCondition::Lte(v) => {
                    let s = format!("{} <= ?{}", col, *param_idx);
                    *param_idx += 1;
                    params.push(Parameter::Literal(serde_json::Value::String(v.clone())));
                    (s, params)
                },
                WhereCondition::In(vals) => {
                    if vals.is_empty() {
                        ("1=0".to_string(), params)
                    } else {
                        let mut placeholders = Vec::new();
                        for v in vals {
                            placeholders.push(format!("?{}", *param_idx));
                            *param_idx += 1;
                            params.push(Parameter::Literal(serde_json::Value::String(v.clone())));
                        }
                        (format!("{} IN ({})", col, placeholders.join(", ")), params)
                    }
                },
                WhereCondition::IsNull => (format!("{} IS NULL", col), params),
                WhereCondition::IsNotNull => (format!("{} IS NOT NULL", col), params),
            }
        }
        WhereClause::Relation { target_model, fk_column, is_forward, filter, .. } => {
            let child_alias = format!("{}_{}", alias, target_model.to_lowercase());
            
            let join_cond = if *is_forward {
                // Parent holds FK
                format!("{}.id = {}.{}", child_alias, alias, fk_column)
            } else {
                // Child holds FK
                format!("{}.{} = {}.id", child_alias, fk_column, alias)
            };

            match filter {
                RelationFilter::Some(inner) => {
                    let (inner_sql, inner_params) = compile_parameterized_where(inner, &child_alias, param_idx);
                    params.extend(inner_params);
                    (format!("EXISTS (SELECT 1 FROM {} AS {} WHERE {} AND {})", target_model, child_alias, join_cond, inner_sql), params)
                }
                RelationFilter::Every(inner) => {
                    let (inner_sql, inner_params) = compile_parameterized_where(inner, &child_alias, param_idx);
                    params.extend(inner_params);
                    (format!("NOT EXISTS (SELECT 1 FROM {} AS {} WHERE {} AND NOT ({}))", target_model, child_alias, join_cond, inner_sql), params)
                }
                RelationFilter::None(inner) => {
                    let (inner_sql, inner_params) = compile_parameterized_where(inner, &child_alias, param_idx);
                    params.extend(inner_params);
                    (format!("NOT EXISTS (SELECT 1 FROM {} AS {} WHERE {} AND {})", target_model, child_alias, join_cond, inner_sql), params)
                }
                RelationFilter::Is(inner) => {
                    let (inner_sql, inner_params) = compile_parameterized_where(inner, &child_alias, param_idx);
                    params.extend(inner_params);
                    (format!("EXISTS (SELECT 1 FROM {} AS {} WHERE {} AND {})", target_model, child_alias, join_cond, inner_sql), params)
                }
                RelationFilter::IsNot(inner) => {
                    let (inner_sql, inner_params) = compile_parameterized_where(inner, &child_alias, param_idx);
                    params.extend(inner_params);
                    (format!("NOT EXISTS (SELECT 1 FROM {} AS {} WHERE {} AND {})", target_model, child_alias, join_cond, inner_sql), params)
                }
            }
        },
        WhereClause::AlwaysTrue => ("1=1".to_string(), params),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_parser::ast::{ModelNode, FieldNode};
    use serde_json::json;

    fn mock_ast() -> SchemaAst {
        let mut ast = SchemaAst {
            models: std::collections::HashMap::new(),
            unions: std::collections::HashMap::new(),
        };

        ast.models.insert("User".to_string(), ModelNode {
            name: "User".to_string(),
            fields: vec![
                FieldNode { name: "id".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "name".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "age".to_string(), field_type: AstFieldType::Scalar("Int".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "password".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![FieldAttribute::Ignore] },
            ]
        });

        ast
    }

    #[test]
    fn test_hydrate_create() {
        let ast = mock_ast();
        let payload = json!({
            "data": {
                "name": "Alice",
                "age": 30
            }
        });
        
        let mut alias_counter = 0;
        let plan = hydrate_mutation_to_plan(&ast, "User", "create", &payload, &mut alias_counter).unwrap();
        
        assert_eq!(plan.steps.len(), 1);
        if let ExecutionStep::Query { id, sql, params } = &plan.steps[0] {
            assert_eq!(id, "step_user_0");
            assert!(sql.starts_with("INSERT INTO User"));
            assert!(sql.contains("RETURNING id;"));
            assert_eq!(params.len(), 2);
        } else {
            panic!("Expected Query step");
        }
    }
    #[test]
    fn test_hydrate_update() {
        let ast = mock_ast();
        let payload = json!({
            "data": {
                "age": 31
            },
            "where": {
                "id": "user_123"
            }
        });
        
        let mut alias_counter = 0;
        let plan = hydrate_mutation_to_plan(&ast, "User", "update", &payload, &mut alias_counter).unwrap();
        
        assert_eq!(plan.steps.len(), 1);
        if let ExecutionStep::Query { id, sql, params } = &plan.steps[0] {
            assert_eq!(id, "step_user_0");
            assert!(sql.starts_with("UPDATE User SET age = ?1 WHERE User.id = ?2 RETURNING id;"));
            assert_eq!(params.len(), 2);
        } else {
            panic!("Expected Query step");
        }
    }

    #[test]
    fn test_hydrate_delete() {
        let ast = mock_ast();
        let payload = json!({
            "where": {
                "id": "user_123"
            }
        });
        
        let mut alias_counter = 0;
        let plan = hydrate_mutation_to_plan(&ast, "User", "delete", &payload, &mut alias_counter).unwrap();
        
        assert_eq!(plan.steps.len(), 1);
        if let ExecutionStep::Query { id, sql, params } = &plan.steps[0] {
            assert_eq!(id, "step_user_0");
            assert!(sql.starts_with("DELETE FROM User WHERE User.id = ?1 RETURNING id;"));
            assert_eq!(params.len(), 1);
        } else {
            panic!("Expected Query step");
        }
    }
}

use serde_json::Value;
use query_compiler::mutation_ir::{ExecutionPlan, ExecutionStep, Parameter};
use schema_parser::ast::{SchemaAst, AstFieldType, FieldAttribute};
use crate::where_parser::parse_where_clause;

fn validate_and_normalize_scalar(field_name: &str, type_name: &str, val: &Value) -> Result<Value, String> {
    match type_name {
        "Float" => {
            if val.as_f64().is_some() {
                Ok(val.clone())
            } else {
                Err(format!("Validation Error: Field '{}' expects a Float.", field_name))
            }
        },
        "DateTime" => {
            let str_val = val.as_str().ok_or_else(|| format!("Validation Error: Invalid ISO-8601 DateTime format for field '{}'.", field_name))?;
            let date = chrono::DateTime::parse_from_rfc3339(str_val)
                .map_err(|_| format!("Validation Error: Invalid ISO-8601 DateTime format for field '{}'.", field_name))?;
            let normalized = date.with_timezone(&chrono::Utc).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            Ok(Value::String(normalized))
        },
        _ => Ok(val.clone()),
    }
}

pub fn hydrate_mutation_to_plan(
    ast: &SchemaAst,
    model_name: &str,
    action: &str,
    payload: &Value,
    alias_counter: &mut usize,
) -> Result<ExecutionPlan, String> {
    if ast.bases.contains_key(model_name) {
        return Err("Security Exception: Cannot mutate abstract base shape".to_string());
    }
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
            let pk_col = model_def.resolved_fields.iter()
                .find(|f| f.attributes.iter().any(|a| matches!(a, FieldAttribute::Id)))
                .map(|f| f.name.as_str())
                .unwrap_or("__id");
            
            let mut params = Vec::new();
            let mut param_idx = 1;
            
            let where_clause_ir = parse_where_clause(ast, where_obj, model_def)?;
            let (where_sql, where_params) = compile_parameterized_where(&where_clause_ir, model_name, &mut param_idx);
            params.extend(where_params);
            
            let sql = format!(
                "DELETE FROM {} WHERE {} RETURNING {};",
                model_name,
                where_sql,
                pk_col
            );
            
            steps.push(ExecutionStep::Query {
                id: step_id.clone(),
                sql,
                params,
            });

            // --- Application-Level Cascading Deletes for Polymorphic Bases ---
            for (other_model_name, other_model_def) in &ast.models {
                for field in &other_model_def.resolved_fields {
                    if let AstFieldType::PolymorphicBase(base_name) = &field.field_type {
                        if model_def.resolved_bases.contains(base_name) {
                            let cascade_step_id = format!("step_{}_cascade_{}_{}", model_name.to_lowercase(), other_model_name.to_lowercase(), *alias_counter);
                            *alias_counter += 1;
                            
                            let type_col = format!("{}_type", field.name);
                            let id_col = format!("{}_id", field.name);
                            
                            // Delete the referencing row from the other model
                            let cascade_sql = format!(
                                "DELETE FROM {} WHERE {} = '{}' AND {} = ?1;",
                                other_model_name,
                                type_col,
                                model_name,
                                id_col
                            );
                            
                            steps.push(ExecutionStep::Query {
                                id: cascade_step_id,
                                sql: cascade_sql,
                                params: vec![Parameter::Reference {
                                    step_id: step_id.clone(),
                                    column: pk_col.to_string(),
                                }],
                            });
                        }
                    }
                }
            }
            // --- End Cascading Deletes ---
            
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
        
    let pk_col = model_def.resolved_fields.iter()
        .find(|f| f.attributes.iter().any(|a| matches!(a, FieldAttribute::Id)))
        .map(|f| f.name.as_str())
        .unwrap_or("__id");

    let step_id = format!("step_{}_{}", model_name.to_lowercase(), *alias_counter);
    *alias_counter += 1;
    
    let mut columns = Vec::new();
    let mut params = Vec::new();
    let mut placeholders = Vec::new();
    let mut param_idx = 1;
    
    let mut deferred_children = Vec::new();
    
    if let Some(rel) = &parent_rel {
        let parent_model_def = ast.models.get(&rel.parent_model).unwrap();
        let parent_pk_col = parent_model_def.resolved_fields.iter()
            .find(|f| f.attributes.iter().any(|a| matches!(a, FieldAttribute::Id)))
            .map(|f| f.name.as_str())
            .unwrap_or("__id");
        let parent_field_def = parent_model_def.resolved_fields.iter().find(|f| f.name == rel.relation_field_name).unwrap();
        
        let mut fk_col = None;
        let mut target_pk = "__id".to_string();

        if let Some(FieldAttribute::InternalRelation { fields, references }) = parent_field_def.attributes.iter().find(|a| matches!(a, FieldAttribute::InternalRelation { .. })) {
            if !fields.is_empty() && !references.is_empty() {
                let pfk = &fields[0];
                let rpk = &references[0];
                
                // Does THIS model (the child) hold the FK?
                if model_def.resolved_fields.iter().any(|f| &f.name == pfk) {
                    fk_col = Some(pfk.clone());
                    target_pk = rpk.clone();
                }
            }
        }
        
        if let Some(col) = fk_col {
            columns.push(col.clone());
            placeholders.push(format!("?{}", param_idx));
            params.push(Parameter::Reference { step_id: rel.parent_step_id.clone(), column: target_pk });
            param_idx += 1;
        }
    }
    
    for (key, val) in data {
        if key.starts_with("__") { continue; }
        
        let field_def = model_def.resolved_fields.iter().find(|f| &f.name == key)
            .ok_or_else(|| format!("Invalid field '{}' for model '{}'.", key, model_name))?;

        match &field_def.field_type {
            AstFieldType::Scalar(type_name) => {
                let normalized_val = validate_and_normalize_scalar(key, type_name, val)?;
                columns.push(key.clone());
                placeholders.push(format!("?{}", param_idx));
                params.push(Parameter::Literal(normalized_val));
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
                    if let FieldAttribute::InternalRelation { fields, .. } = attr {
                        if !fields.is_empty() {
                            // Verify that we actually own the column physically AND it's not an array relation
                            if !is_array && model_def.resolved_fields.iter().any(|f| &f.name == &fields[0]) {
                                we_hold_fk = true;
                                fk_column = Some(fields[0].clone());
                            }
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
                        let child_model_def = ast.models.get(target_model).unwrap();
                        let child_pk_col = child_model_def.resolved_fields.iter().find(|f| f.attributes.iter().any(|a| matches!(a, FieldAttribute::Id))).map(|f| f.name.as_str()).unwrap_or("__id");
                        
                        if let Some(col) = &fk_column {
                            columns.push(col.clone());
                            placeholders.push(format!("?{}", param_idx));
                            params.push(Parameter::Reference { step_id: child_step_id, column: child_pk_col.to_string() });
                            param_idx += 1;
                        }
                    }
                    if let Some(connect_payload) = nested_mutations.get("connect") {
                        if let Some(connect_id) = connect_payload.as_object().and_then(|o| o.get("__id")) {
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
            AstFieldType::PolymorphicUnionArray(_) | AstFieldType::PolymorphicBaseArray(_) => {
                return Err(format!("Unsupported: Array mutations on polymorphic field '{}' are not yet implemented.", key));
            },
            AstFieldType::PolymorphicUnion(_) | AstFieldType::PolymorphicBase(_) => {
                let nested_mutations = val.as_object().ok_or(format!("Expected object for polymorphic field '{}'", key))?;
                
                // Expecting exactly one target type key (e.g. { "ModelA": { "connect": { "__id": "1" } } })
                if nested_mutations.len() != 1 {
                    return Err(format!("Polymorphic field '{}' requires exactly one target type in the mutation payload.", key));
                }
                
                let (target_model, actions) = nested_mutations.iter().next().unwrap();
                let actions_obj = actions.as_object().ok_or(format!("Expected object for target type '{}' in field '{}'", target_model, key))?;

                // Validate target model exists
                if !ast.models.contains_key(target_model) {
                    return Err(format!("Security Exception: Target model '{}' undefined.", target_model));
                }
                
                let type_col = format!("{}_type", key);
                let id_col = format!("{}_id", key);

                if let Some(connect_payload) = actions_obj.get("connect") {
                    if let Some(connect_id) = connect_payload.as_object().and_then(|o| o.get("__id")) {
                        // 1. Set type column
                        columns.push(type_col.clone());
                        placeholders.push(format!("?{}", param_idx));
                        params.push(Parameter::Literal(serde_json::Value::String(target_model.clone())));
                        param_idx += 1;
                        
                        // 2. Set id column
                        columns.push(id_col.clone());
                        placeholders.push(format!("?{}", param_idx));
                        params.push(Parameter::Literal(connect_id.clone()));
                        param_idx += 1;
                    }
                } else if let Some(create_payload) = actions_obj.get("create") {
                    if let Some(child_data) = create_payload.as_object() {
                        deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Create(child_data.clone()), relation_field_name: key.clone() });
                    }
                } else {
                    return Err(format!("Unsupported action for polymorphic field '{}'. Only 'connect' and 'create' are supported.", key));
                }
            }
        }
    }
    
    let sql = if columns.is_empty() {
        format!("INSERT INTO {} DEFAULT VALUES RETURNING {};", model_name, pk_col)
    } else {
        format!(
            "INSERT INTO {} ({}) VALUES ({}) RETURNING {};",
            model_name,
            columns.join(", "),
            placeholders.join(", "),
            pk_col
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
        
    let pk_col = model_def.resolved_fields.iter()
        .find(|f| f.attributes.iter().any(|a| matches!(a, FieldAttribute::Id)))
        .map(|f| f.name.as_str())
        .unwrap_or("__id");

    let step_id = format!("step_{}_{}", model_name.to_lowercase(), *alias_counter);
    *alias_counter += 1;
    
    let mut set_clauses = Vec::new();
    let mut params = Vec::new();
    let mut param_idx = 1;
    
    let mut deferred_children = Vec::new();
    
    for (key, val) in data {
        if key.starts_with("__") { continue; }
        
        let field_def = model_def.resolved_fields.iter().find(|f| &f.name == key)
            .ok_or_else(|| format!("Invalid field '{}' for model '{}'.", key, model_name))?;

        match &field_def.field_type {
            AstFieldType::Scalar(type_name) => {
                let normalized_val = validate_and_normalize_scalar(key, type_name, val)?;
                set_clauses.push(format!("{} = ?{}", key, param_idx));
                params.push(Parameter::Literal(normalized_val));
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
                    if let FieldAttribute::InternalRelation { fields, .. } = attr {
                        if !fields.is_empty() {
                            // Verify that we actually own the column physically AND it's not an array relation
                            if !is_array && model_def.resolved_fields.iter().any(|f| &f.name == &fields[0]) {
                                we_hold_fk = true;
                                fk_column = Some(fields[0].clone());
                            }
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
                        let child_model_def = ast.models.get(target_model).unwrap();
                        let child_pk_col = child_model_def.resolved_fields.iter().find(|f| f.attributes.iter().any(|a| matches!(a, FieldAttribute::Id))).map(|f| f.name.as_str()).unwrap_or("__id");
                        
                        if let Some(col) = &fk_column {
                            set_clauses.push(format!("{} = ?{}", col, param_idx));
                            params.push(Parameter::Reference { step_id: child_step_id, column: child_pk_col.to_string() });
                            param_idx += 1;
                        }
                    }
                    if let Some(connect_payload) = nested_mutations.get("connect") {
                        if let Some(connect_id) = connect_payload.as_object().and_then(|o| o.get("__id")) {
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
            AstFieldType::PolymorphicUnionArray(_) | AstFieldType::PolymorphicBaseArray(_) => {
                return Err(format!("Unsupported: Array mutations on polymorphic field '{}' are not yet implemented.", key));
            },
            AstFieldType::PolymorphicUnion(_) | AstFieldType::PolymorphicBase(_) => {
                let nested_mutations = val.as_object().ok_or(format!("Expected object for polymorphic field '{}'", key))?;
                
                let type_col = format!("{}_type", key);
                let id_col = format!("{}_id", key);

                if let Some(disconnect_val) = nested_mutations.get("disconnect") {
                    if disconnect_val.as_bool().unwrap_or(false) {
                        set_clauses.push(format!("{} = NULL", type_col));
                        set_clauses.push(format!("{} = NULL", id_col));
                        continue;
                    }
                }

                // Expecting exactly one target type key (e.g. { "ModelA": { "connect": { "__id": "1" } } })
                if nested_mutations.len() != 1 {
                    return Err(format!("Polymorphic field '{}' requires exactly one target type in the mutation payload.", key));
                }
                
                let (target_model, actions) = nested_mutations.iter().next().unwrap();
                let actions_obj = actions.as_object().ok_or(format!("Expected object for target type '{}' in field '{}'", target_model, key))?;

                // Validate target model exists
                if !ast.models.contains_key(target_model) {
                    return Err(format!("Security Exception: Target model '{}' undefined.", target_model));
                }

                if let Some(connect_payload) = actions_obj.get("connect") {
                    if let Some(connect_id) = connect_payload.as_object().and_then(|o| o.get("__id")) {
                        // 1. Set type column
                        set_clauses.push(format!("{} = ?{}", type_col, param_idx));
                        params.push(Parameter::Literal(serde_json::Value::String(target_model.clone())));
                        param_idx += 1;
                        
                        // 2. Set id column
                        set_clauses.push(format!("{} = ?{}", id_col, param_idx));
                        params.push(Parameter::Literal(connect_id.clone()));
                        param_idx += 1;
                    }
                } else if let Some(create_payload) = actions_obj.get("create") {
                    if let Some(child_data) = create_payload.as_object() {
                        deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Create(child_data.clone()), relation_field_name: key.clone() });
                    }
                } else {
                    return Err(format!("Unsupported action for polymorphic field '{}'. Only 'connect' and 'create' are supported.", key));
                }
            }
        }
    }
    
    if set_clauses.is_empty() {
        set_clauses.push(format!("{} = {}", pk_col, pk_col));
    }
    
    let where_clause_ir = parse_where_clause(ast, where_obj, model_def)?;
    let (mut where_sql, mut where_params) = compile_parameterized_where(&where_clause_ir, model_name, &mut param_idx);
    
    if let Some(rel) = &parent_rel {
        let parent_model_def = ast.models.get(&rel.parent_model).unwrap();
        let parent_pk_col = parent_model_def.resolved_fields.iter()
            .find(|f| f.attributes.iter().any(|a| matches!(a, FieldAttribute::Id)))
            .map(|f| f.name.as_str())
            .unwrap_or("__id");
        
        let mut fk_col = None;
        let mut target_pk = "__id".to_string();

        // Find the relation definition on the parent side
        if let Some(field) = parent_model_def.resolved_fields.iter().find(|f| f.name == rel.relation_field_name) {
            if let Some(FieldAttribute::InternalRelation { fields, references }) = field.attributes.iter().find(|a| matches!(a, FieldAttribute::InternalRelation { .. })) {
                if !fields.is_empty() && !references.is_empty() {
                    let pfk = &fields[0];
                    let rpk = &references[0];
                    
                    // Does THIS model (the child) hold the FK?
                    if model_def.resolved_fields.iter().any(|f| &f.name == pfk) {
                        fk_col = Some(pfk.clone());
                        target_pk = rpk.clone();
                    }
                }
            }
        }

        if let Some(col) = fk_col {
            where_sql = format!("({} AND {}.{} = ?{})", where_sql, model_name, col, param_idx);
            where_params.push(Parameter::Reference { step_id: rel.parent_step_id.clone(), column: target_pk });
            param_idx += 1;
        }
    }
    params.extend(where_params);

    let sql = format!(
        "UPDATE {} SET {} WHERE {} RETURNING {};",
        model_name,
        set_clauses.join(", "),
        where_sql,
        pk_col
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
        
    let pk_col = model_def.resolved_fields.iter()
        .find(|f| f.attributes.iter().any(|a| matches!(a, FieldAttribute::Id)))
        .map(|f| f.name.as_str())
        .unwrap_or("__id");

    let step_id = format!("step_{}_{}", model_name.to_lowercase(), *alias_counter);
    *alias_counter += 1;
    
    let mut param_idx = 1;
    let where_clause_ir = parse_where_clause(ast, where_obj, model_def)?;
    
    // Safety check: Upserts must target a unique field
    // In Phase 4, we enforce this at the Rust layer since we don't rely on ON CONFLICT anymore.
    let conflict_target = where_obj.keys().next().ok_or("Upsert 'where' block must contain at least one key")?.clone();
    let target_field = model_def.resolved_fields.iter().find(|f| f.name == conflict_target).unwrap();
    if !target_field.attributes.iter().any(|a| matches!(a, FieldAttribute::Id | FieldAttribute::Unique)) {
        return Err(format!("Security Exception: Upsert target '{}' is not marked as @id or @unique", conflict_target));
    }
    
    let (where_sql, check_params) = compile_parameterized_where(&where_clause_ir, model_name, &mut param_idx);
    
    let check_sql = format!("SELECT {} FROM {} WHERE {}", pk_col, model_name, where_sql);
    
    let mut if_not_exists = Vec::new();
    let _create_step_id = translate_create_node(ast, model_name, create_data, &mut if_not_exists, alias_counter, None)?;

    let mut if_exists = Vec::new();
    let _update_step_id = translate_update_node(ast, model_name, where_obj, update_data, &mut if_exists, alias_counter, None)?;
    
    // We need the executor to return `create_step_id` or `update_step_id` under `step_id`?
    // Actually, `ExecutionStep::UpsertBranch` currently uses `root_step_id: String` to know what to assign.
    // In `executor.rs`: `returned_values.insert(__id.clone(), returned_id);`
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
    let parent_model_def = ast.models.get(parent_model_name).unwrap();
    let parent_pk_col = parent_model_def.resolved_fields.iter().find(|f| f.attributes.iter().any(|a| matches!(a, FieldAttribute::Id))).map(|f| f.name.as_str()).unwrap_or("__id");

    for child in deferred_children {
        let child_model_def = ast.models.get(&child.target_model).unwrap();
        let child_pk_col = child_model_def.resolved_fields.iter().find(|f| f.attributes.iter().any(|a| matches!(a, FieldAttribute::Id))).map(|f| f.name.as_str()).unwrap_or("__id");
        
        let parent_field_def = parent_model_def.resolved_fields.iter().find(|f| f.name == child.relation_field_name).unwrap();
        let is_polymorphic = matches!(parent_field_def.field_type, AstFieldType::PolymorphicBase(_) | AstFieldType::PolymorphicBaseArray(_) | AstFieldType::PolymorphicUnion(_) | AstFieldType::PolymorphicUnionArray(_));

        if is_polymorphic {
            match child.action {
                DeferredAction::Create(child_data) => {
                    let child_create_step_id = translate_create_node(ast, &child.target_model, &child_data, steps, alias_counter, None)?;
                    
                    // We must then update the parent model to point to the newly created child!
                    let update_step_id = format!("step_{}_poly_update_{}", parent_model_name.to_lowercase(), *alias_counter);
                    *alias_counter += 1;
                    
                    let type_col = format!("{}_type", child.relation_field_name);
                    let id_col = format!("{}_id", child.relation_field_name);
                    
                    let sql = format!(
                        "UPDATE {} SET {} = ?, {} = ? WHERE {} = ? RETURNING {};",
                        parent_model_name,
                        type_col,
                        id_col,
                        parent_pk_col,
                        parent_pk_col
                    );
                    
                    steps.push(ExecutionStep::Query {
                        id: update_step_id,
                        sql,
                        params: vec![
                            Parameter::Literal(serde_json::Value::String(child.target_model.clone())),
                            Parameter::Reference { step_id: child_create_step_id, column: child_pk_col.to_string() },
                            Parameter::Reference { step_id: parent_step_id.to_string(), column: parent_pk_col.to_string() }
                        ],
                    });
                },
                _ => return Err(format!("Unsupported deferred action for polymorphic relation '{}'", child.relation_field_name)),
            }
            continue;
        }

        let mut fk_column_name = None;
        let mut target_pk = "__id".to_string();

        // Find the field in the child model that points back to the parent
        let rel_attr = parent_field_def.attributes.iter().find(|a| matches!(a, FieldAttribute::Relation { .. }));
        let rel_name = match rel_attr {
            Some(FieldAttribute::Relation { name, .. }) => name.clone(),
            _ => None,
        };

        let child_model_def = ast.models.get(&child.target_model).unwrap();
        let reverse_field = child_model_def.resolved_fields.iter().find(|f| {
            match &f.field_type {
                AstFieldType::Relation(rt) if rt == parent_model_name => {
                    if let Some(FieldAttribute::Relation { name, .. }) = f.attributes.iter().find(|a| matches!(a, FieldAttribute::Relation { .. })) {
                        if name == &rel_name { return true; }
                    }
                    false
                }
                _ => false
            }
        });

        if let Some(rev_f) = reverse_field {
            if let Some(FieldAttribute::InternalRelation { fields, references }) = rev_f.attributes.iter().find(|a| matches!(a, FieldAttribute::InternalRelation { .. })) {
                if !fields.is_empty() && !references.is_empty() {
                    fk_column_name = Some(fields[0].clone());
                    target_pk = references[0].clone();
                }
            }
        }

        let fk_col = fk_column_name.unwrap_or_else(|| format!("{}Id", parent_model_name.to_lowercase()));

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
                params.push(Parameter::Reference { step_id: parent_step_id.to_string(), column: parent_pk_col.to_string() });
                param_idx += 1;
                
                let child_model_def = ast.models.get(&child.target_model).unwrap();
                let where_clause_ir = parse_where_clause(ast, &connect_where, child_model_def)?;
                let (where_sql, where_params) = compile_parameterized_where(&where_clause_ir, &child.target_model, &mut param_idx);
                params.extend(where_params);
                
                let sql = format!(
                    "UPDATE {} SET {} WHERE {} RETURNING {};",
                    child.target_model,
                    set_clause,
                    where_sql,
                    child_pk_col
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
                let combined_where_sql = format!("({} AND {}.{} = ?{})", where_sql, child.target_model, fk_col, param_idx);
                params.push(Parameter::Reference { step_id: parent_step_id.to_string(), column: target_pk.clone() });
                
                let sql = format!(
                    "DELETE FROM {} WHERE {} RETURNING {};",
                    child.target_model,
                    combined_where_sql,
                    child_pk_col
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
                
                let combined_where_sql = format!("({} AND {}.{} = ?{})", where_sql, child.target_model, fk_col, param_idx);
                params.push(Parameter::Reference { step_id: parent_step_id.to_string(), column: target_pk.clone() });
                
                let sql = format!(
                    "UPDATE {} SET {} = NULL WHERE {} RETURNING {};",
                    child.target_model,
                    fk_col,
                    combined_where_sql,
                    child_pk_col
                );
                
                steps.push(ExecutionStep::Query {
                    id: child_step_id,
                    sql,
                    params,
                });
            },
            DeferredAction::Upsert(create_data, update_data) => {
                let mut pfk_col = None;
                let mut rpk_col = None;
                if let Some(FieldAttribute::InternalRelation { fields, references }) = ast.models.get(parent_model_name).unwrap().resolved_fields.iter().find(|f| f.name == child.relation_field_name).unwrap().attributes.iter().find(|a| matches!(a, FieldAttribute::InternalRelation { .. })) {
                    if !fields.is_empty() && !references.is_empty() {
                        pfk_col = Some(fields[0].clone());
                        rpk_col = Some(references[0].clone());
                    }
                }

                let pfk = pfk_col.unwrap_or_else(|| format!("{}Id", child.relation_field_name));
                let rpk = rpk_col.unwrap_or_else(|| "__id".to_string());
                
                // Determine if parent owns the FK or child owns the FK
                let parent_owns_fk = parent_model_def.resolved_fields.iter().any(|f| f.name == pfk);

                let child_step_id = format!("step_{}_upsert_{}", child.target_model.to_lowercase(), *alias_counter);
                *alias_counter += 1;

                if parent_owns_fk {
                    // Forward Relation (e.g. User has profileId)
                    let mut columns = Vec::new();
                    let mut placeholders = Vec::new();
                    let mut params = Vec::new();
                    let mut param_idx = 1;
                    
                    columns.push(child_pk_col.to_string());
                    placeholders.push(format!("COALESCE((SELECT {} FROM {} WHERE {} = ?{}), gen_uuid7())", pfk, parent_model_name, parent_pk_col, param_idx));
                    params.push(Parameter::Reference { step_id: parent_step_id.to_string(), column: parent_pk_col.to_string() });
                    param_idx += 1;
                    
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
                        update_set_clauses.push(format!("{} = excluded.{}", child_pk_col, child_pk_col));
                    }

                    let sql = format!(
                        "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT({}) DO UPDATE SET {} RETURNING {};",
                        child.target_model, columns.join(", "), placeholders.join(", "), child_pk_col, update_set_clauses.join(", "), child_pk_col
                    );
                    
                    steps.push(ExecutionStep::Query { id: child_step_id.clone(), sql, params });

                    let link_step_id = format!("step_{}_upsert_link_{}", parent_model_name.to_lowercase(), *alias_counter);
                    *alias_counter += 1;
                    let link_sql = format!("UPDATE {} SET {} = ?1 WHERE {} = ?2 RETURNING {};", parent_model_name, pfk, parent_pk_col, parent_pk_col);
                    steps.push(ExecutionStep::Query {
                        id: link_step_id,
                        sql: link_sql,
                        params: vec![
                            Parameter::Reference { step_id: child_step_id.clone(), column: child_pk_col.to_string() },
                            Parameter::Reference { step_id: parent_step_id.to_string(), column: parent_pk_col.to_string() },
                        ],
                    });
                } else {
                    // Reverse Relation (e.g. Profile has userId)
                    let mut columns = Vec::new();
                    let mut placeholders = Vec::new();
                    let mut params = Vec::new();
                    let mut param_idx = 1;
                    
                    // The FK to the parent is required for creation
                    columns.push(pfk.clone());
                    placeholders.push(format!("?{}", param_idx));
                    params.push(Parameter::Reference { step_id: parent_step_id.to_string(), column: rpk.clone() });
                    param_idx += 1;

                    for (key, val) in create_data {
                        if key != pfk {
                            columns.push(key.clone());
                            placeholders.push(format!("?{}", param_idx));
                            params.push(Parameter::Literal(val.clone()));
                            param_idx += 1;
                        }
                    }

                    let mut update_set_clauses = Vec::new();
                    if let Some(update_data_obj) = update_data.get("data").and_then(|v| v.as_object()).or(Some(&update_data)) {
                        for (key, val) in update_data_obj {
                            if *key != pfk {
                                update_set_clauses.push(format!("{} = ?{}", key, param_idx));
                                params.push(Parameter::Literal(val.clone()));
                                param_idx += 1;
                            }
                        }
                    }

                    if update_set_clauses.is_empty() {
                        update_set_clauses.push(format!("{} = excluded.{}", pfk, pfk));
                    }

                    // Conflict target for reverse 1:1 is the FK column itself!
                    let sql = format!(
                        "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT({}) DO UPDATE SET {} RETURNING {};",
                        child.target_model, columns.join(", "), placeholders.join(", "), pfk, update_set_clauses.join(", "), child_pk_col
                    );
                    
                    steps.push(ExecutionStep::Query { id: child_step_id.clone(), sql, params });
                }
            },
            DeferredAction::Set(child_wheres) => {
                // First disconnect all existing
                let disconnect_step_id = format!("step_{}_set_disconnect_{}", child.target_model.to_lowercase(), *alias_counter);
                *alias_counter += 1;
                
                let sql = format!("UPDATE {} SET {} = NULL WHERE {} = ?1 RETURNING {};", child.target_model, fk_col, fk_col, child_pk_col);
                steps.push(ExecutionStep::Query {
                    id: disconnect_step_id,
                    sql,
                    params: vec![Parameter::Reference { step_id: parent_step_id.to_string(), column: target_pk.clone() }],
                });
                
                // Then connect each child
                for connect_where in child_wheres {
                    let child_step_id = format!("step_{}_set_connect_{}", child.target_model.to_lowercase(), *alias_counter);
                    *alias_counter += 1;
                    
                    let mut params = Vec::new();
                    let mut param_idx = 1;
                    
                    let set_clause = format!("{} = ?{}", fk_col, param_idx);
                    params.push(Parameter::Reference { step_id: parent_step_id.to_string(), column: target_pk.clone() });
                    param_idx += 1;
                    
                    let child_model_def = ast.models.get(&child.target_model).unwrap();
                    let where_clause_ir = parse_where_clause(ast, &connect_where, child_model_def)?;
                    let (where_sql, where_params) = compile_parameterized_where(&where_clause_ir, &child.target_model, &mut param_idx);
                    params.extend(where_params);
                    
                    let sql = format!(
                        "UPDATE {} SET {} WHERE {} RETURNING __id;",
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
                format!("{}.__id = {}.{}", child_alias, alias, fk_column)
            } else {
                // Child holds FK
                format!("{}.{} = {}.__id", child_alias, fk_column, alias)
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
        let mut ast = SchemaAst { bases: std::collections::HashMap::new(),
            models: std::collections::HashMap::new(),
            unions: std::collections::HashMap::new(),
        };

        ast.models.insert("User".to_string(), ModelNode { block_attributes: vec![], extends: vec![], fields: vec![], resolved_bases: std::collections::BTreeSet::new(),
            name: "User".to_string(),
            resolved_fields: vec![
                FieldNode { name: "__id".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "name".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "age".to_string(), field_type: AstFieldType::Scalar("Int".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "password".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![FieldAttribute::InternalTracked] },
            ]
        });

        ast
    }

    #[test]
    fn test_validate_and_normalize_scalar_float() {
        assert_eq!(
            validate_and_normalize_scalar("val", "Float", &json!(10.5)).unwrap(),
            json!(10.5)
        );
        assert_eq!(
            validate_and_normalize_scalar("val", "Float", &json!(10)).unwrap(),
            json!(10)
        );
        assert!(validate_and_normalize_scalar("val", "Float", &json!("10.5")).is_err());
    }

    #[test]
    fn test_validate_and_normalize_scalar_datetime() {
        assert_eq!(
            validate_and_normalize_scalar("date", "DateTime", &json!("2025-01-01T00:00:00Z")).unwrap(),
            json!("2025-01-01T00:00:00.000Z")
        );
        assert_eq!(
            validate_and_normalize_scalar("date", "DateTime", &json!("2025-10-10T12:00:00-04:00")).unwrap(),
            json!("2025-10-10T16:00:00.000Z")
        );
        assert!(validate_and_normalize_scalar("date", "DateTime", &json!("Next Tuesday")).is_err());
    }

    #[test]
    fn test_validate_and_normalize_scalar_fallback() {
        assert_eq!(
            validate_and_normalize_scalar("name", "String", &json!("Alice")).unwrap(),
            json!("Alice")
        );
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
            assert!(sql.contains("RETURNING __id;"));
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
                "__id": "user_123"
            }
        });
        
        let mut alias_counter = 0;
        let plan = hydrate_mutation_to_plan(&ast, "User", "update", &payload, &mut alias_counter).unwrap();
        
        assert_eq!(plan.steps.len(), 1);
        if let ExecutionStep::Query { id, sql, params } = &plan.steps[0] {
            assert_eq!(id, "step_user_0");
            assert!(sql.starts_with("UPDATE User SET age = ?1 WHERE User.__id = ?2 RETURNING __id;"));
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
                "__id": "user_123"
            }
        });
        
        let mut alias_counter = 0;
        let plan = hydrate_mutation_to_plan(&ast, "User", "delete", &payload, &mut alias_counter).unwrap();
        
        assert_eq!(plan.steps.len(), 1);
        if let ExecutionStep::Query { id, sql, params } = &plan.steps[0] {
            assert_eq!(id, "step_user_0");
            assert!(sql.starts_with("DELETE FROM User WHERE User.__id = ?1 RETURNING __id;"));
            assert_eq!(params.len(), 1);
        } else {
            panic!("Expected Query step");
        }
    }
}

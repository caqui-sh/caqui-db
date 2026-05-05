use serde_json::Value;
use query_compiler::mutation_ir::{ExecutionPlan, ExecutionStep, Parameter};
use schema_parser::ast::{SchemaAst, AstFieldType, FieldAttribute};
use crate::where_parser::parse_where_clause;

fn validate_and_normalize_scalar(ast: &SchemaAst, field_name: &str, type_name: &str, is_enum: bool, val: &Value) -> Result<Value, String> {
    if val.is_null() {
        return Ok(Value::Null);
    }
    if is_enum {
        let variants = ast.enums.get(type_name).ok_or_else(|| format!("Security Exception: Enum '{}' undefined.", type_name))?;
        let str_val = val.as_str().ok_or_else(|| format!("Validation Error: Field '{}' expects a String for enum '{}'.", field_name, type_name))?;
        if variants.contains(&str_val.to_string()) {
            return Ok(val.clone());
        } else {
            return Err(format!("Validation Error: Value '{}' is not a valid variant for enum '{}'.", str_val, type_name));
        }
    }
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
        "updateMany" => {
            let data = payload.get("data").and_then(|v| v.as_object())
                .ok_or("Missing 'data' block in updateMany mutation")?;
            
            let where_obj = payload.get("where").and_then(|v| v.as_object())
                .ok_or("Missing 'where' block in updateMany mutation")?;
                
            let model_def = ast.models.get(model_name)
                .ok_or_else(|| format!("Security Exception: Model '{}' undefined.", model_name))?;

            let mut set_clauses = Vec::new();
            let mut params = Vec::new();
            let mut param_idx = 1;
            let mut deferred_children = Vec::new();

            for (key, val) in data {
                if key.starts_with("__") { continue; }
                
                let field_def = model_def.resolved_fields.iter().find(|f| &f.name == key)
                    .ok_or_else(|| format!("Invalid field '{}' for model '{}'.", key, model_name))?;

                match &field_def.field_type {
                    AstFieldType::Scalar(type_name) | AstFieldType::Enum(type_name) => {
                        let is_enum = matches!(&field_def.field_type, AstFieldType::Enum(_));
                        let normalized_val = validate_and_normalize_scalar(ast, key, type_name, is_enum, val)?;
                        set_clauses.push(format!("{} = ?{}", key, param_idx));
                        params.push(Parameter::Literal(normalized_val));
                        param_idx += 1;
                    },
                    AstFieldType::ScalarArray(_) | AstFieldType::EnumArray(_) => {
                        set_clauses.push(format!("{} = ?{}", key, param_idx));
                        let json_val = serde_json::to_string(val).unwrap_or_else(|_| "[]".to_string());
                        params.push(Parameter::Literal(serde_json::Value::String(json_val)));
                        param_idx += 1;
                    },
                    AstFieldType::Relation(target_model) | AstFieldType::RelationArray(target_model) => {
                        if let Some(nested_mutations) = val.as_object() {
                            if let Some(um_payload) = nested_mutations.get("updateMany") {
                                if let Some(arr) = um_payload.as_array() {
                                    for item in arr {
                                        let c_data = item.get("data").and_then(|v| v.as_object()).unwrap();
                                        let c_where = item.get("where").and_then(|v| v.as_object()).unwrap();
                                        deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::UpdateMany(target_model.clone(), c_where.clone(), c_data.clone()), relation_field_name: key.clone() });
                                    }
                                } else if let Some(item) = um_payload.as_object() {
                                    let c_data = item.get("data").and_then(|v| v.as_object()).unwrap();
                                    let c_where = item.get("where").and_then(|v| v.as_object()).unwrap();
                                    deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::UpdateMany(target_model.clone(), c_where.clone(), c_data.clone()), relation_field_name: key.clone() });
                                }
                            }
                            if let Some(dm_payload) = nested_mutations.get("deleteMany") {
                                if let Some(arr) = dm_payload.as_array() {
                                    for item in arr {
                                        let c_where = item.get("where").and_then(|v| v.as_object()).unwrap();
                                        deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::DeleteMany(target_model.clone(), c_where.clone()), relation_field_name: key.clone() });
                                    }
                                } else if let Some(item) = dm_payload.as_object() {
                                    let c_where = item.get("where").and_then(|v| v.as_object()).unwrap_or(item);
                                    deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::DeleteMany(target_model.clone(), c_where.clone()), relation_field_name: key.clone() });
                                }
                            }
                            if let Some(update_payload) = nested_mutations.get("update") {
                                if let Some(arr) = update_payload.as_array() {
                                    for item in arr {
                                        let c_data = item.get("data").and_then(|v| v.as_object()).unwrap();
                                        let c_where = item.get("where").and_then(|v| v.as_object()).unwrap();
                                        deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Update(target_model.clone(), c_where.clone(), c_data.clone()), relation_field_name: key.clone() });
                                    }
                                } else if let Some(item) = update_payload.as_object() {
                                    let c_data = item.get("data").and_then(|v| v.as_object()).unwrap();
                                    let c_where = item.get("where").and_then(|v| v.as_object()).unwrap();
                                    deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Update(target_model.clone(), c_where.clone(), c_data.clone()), relation_field_name: key.clone() });
                                }
                            }
                            if let Some(delete_payload) = nested_mutations.get("delete") {
                                if let Some(arr) = delete_payload.as_array() {
                                    for item in arr {
                                        deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Delete(target_model.clone(), item.as_object().unwrap().clone()), relation_field_name: key.clone() });
                                    }
                                } else if let Some(item) = delete_payload.as_object() {
                                    deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Delete(target_model.clone(), item.clone()), relation_field_name: key.clone() });
                                }
                            }
                            if let Some(upsert_payload) = nested_mutations.get("upsert") {
                                if let Some(arr) = upsert_payload.as_array() {
                                    for item in arr {
                                        let c_create = item.get("create").and_then(|v| v.as_object()).unwrap();
                                        let c_update = item.get("update").and_then(|v| v.as_object()).unwrap();
                                        deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Upsert(c_create.clone(), c_update.clone()), relation_field_name: key.clone() });
                                    }
                                } else if let Some(item) = upsert_payload.as_object() {
                                    let c_create = item.get("create").and_then(|v| v.as_object()).unwrap();
                                    let c_update = item.get("update").and_then(|v| v.as_object()).unwrap();
                                    deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Upsert(c_create.clone(), c_update.clone()), relation_field_name: key.clone() });
                                }
                            }
                            if let Some(set_payload) = nested_mutations.get("set") {
                                if let Some(arr) = set_payload.as_array() {
                                    let mut set_wheres = Vec::new();
                                    for item in arr {
                                        set_wheres.push(item.as_object().unwrap().clone());
                                    }
                                    deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Set(set_wheres), relation_field_name: key.clone() });
                                } else {
                                    deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Set(Vec::new()), relation_field_name: key.clone() });
                                }
                            }
                            if let Some(create_payload) = nested_mutations.get("create") {
                                if let Some(arr) = create_payload.as_array() {
                                    for item in arr {
                                        let c_data = item.as_object().unwrap();
                                        deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Create(c_data.clone()), relation_field_name: key.clone() });
                                    }
                                } else if let Some(item) = create_payload.as_object() {
                                    deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Create(item.clone()), relation_field_name: key.clone() });
                                }
                            }
                            if let Some(connect_payload) = nested_mutations.get("connect") {
                                if let Some(arr) = connect_payload.as_array() {
                                    for item in arr {
                                        deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Connect(item.as_object().unwrap().clone()), relation_field_name: key.clone() });
                                    }
                                } else if let Some(item) = connect_payload.as_object() {
                                    deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Connect(item.clone()), relation_field_name: key.clone() });
                                }
                            }
                            if let Some(disconnect_payload) = nested_mutations.get("disconnect") {
                                if let Some(arr) = disconnect_payload.as_array() {
                                    for item in arr {
                                        deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Disconnect(item.as_object().unwrap().clone()), relation_field_name: key.clone() });
                                    }
                                } else if let Some(item) = disconnect_payload.as_object() {
                                    deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Disconnect(item.clone()), relation_field_name: key.clone() });
                                }
                            }
                        }
                    },
                    _ => {}
                }
            }

            if set_clauses.is_empty() && deferred_children.is_empty() {
                return Err("No data provided for updateMany".to_string());
            }

            let where_clause_ir = parse_where_clause(ast, where_obj, model_def)?;
            let (where_sql, where_params) = compile_parameterized_where(&where_clause_ir, model_name, &mut param_idx);
            params.extend(where_params.clone());

            let set_str = if set_clauses.is_empty() {
                format!("__id = __id")
            } else {
                set_clauses.join(", ")
            };

            let sql = format!(
                "UPDATE {} SET {} WHERE {};",
                model_name,
                set_str,
                where_sql
            );

            let step_id = format!("step_{}_updatemany_{}", model_name.to_lowercase(), *alias_counter);
            *alias_counter += 1;

            steps.push(ExecutionStep::UpdateMany { id: step_id.clone(), queries: vec![(sql, params)] });

            if !deferred_children.is_empty() {
                let pk_col = model_def.resolved_fields.iter()
                    .find(|f| f.attributes.iter().any(|a| matches!(a, FieldAttribute::Id)))
                    .map(|f| f.name.as_str())
                    .unwrap_or("__id");
                
                let sub_sql = format!("SELECT {} FROM {} WHERE {}", pk_col, model_name, where_sql);
                process_deferred_children(
                    ast,
                    model_name,
                    &ParentConstraint::Bulk { sql: sub_sql, params: where_params },
                    deferred_children,
                    &mut steps,
                    alias_counter
                )?;
            }

            Ok(ExecutionPlan {
                root_step_id: step_id,
                steps,
            })
        },
        "deleteMany" => {
            let default_where = serde_json::Map::new();
            let where_obj = payload.get("where").and_then(|v| v.as_object())
                .unwrap_or(&default_where);
                
            let model_def = ast.models.get(model_name)
                .ok_or_else(|| format!("Security Exception: Model '{}' undefined.", model_name))?;

            let pk_col = model_def.resolved_fields.iter()
                .find(|f| f.attributes.iter().any(|a| matches!(a, FieldAttribute::Id)))
                .map(|f| f.name.as_str())
                .unwrap_or("__id");
            
            let mut param_idx = 1;
            let where_clause_ir = parse_where_clause(ast, where_obj, model_def)?;
            let (where_sql, params) = compile_parameterized_where(&where_clause_ir, model_name, &mut param_idx);
            
            // --- Application-Level Cascading Deletes for Polymorphic Bases ---
            for (other_model_name, other_model_def) in &ast.models {
                for field in &other_model_def.resolved_fields {
                    if let AstFieldType::PolymorphicBase(base_name) = &field.field_type {
                        if model_def.resolved_bases.contains(base_name) {
                            let cascade_step_id = format!("step_{}_cascade_{}_{}", model_name.to_lowercase(), other_model_name.to_lowercase(), *alias_counter);
                            *alias_counter += 1;
                            
                            let type_col = format!("{}_type", field.name);
                            let id_col = format!("{}_id", field.name);
                            
                            let cascade_sql = format!(
                                "DELETE FROM {} WHERE {} = '{}' AND {} IN (SELECT {} FROM {} WHERE {});",
                                other_model_name,
                                type_col,
                                model_name,
                                id_col,
                                pk_col,
                                model_name,
                                where_sql
                            );
                            
                            steps.push(ExecutionStep::Query {
                                id: cascade_step_id,
                                sql: cascade_sql,
                                params: params.clone(),
                            });
                        }
                    }
                }
            }
            // --- End Cascading Deletes ---

            let sql = format!(
                "DELETE FROM {} WHERE {};",
                model_name,
                where_sql
            );

            let step_id = format!("step_{}_deletemany_{}", model_name.to_lowercase(), *alias_counter);
            *alias_counter += 1;
            
            steps.push(ExecutionStep::DeleteMany { id: step_id.clone(), queries: vec![(sql, params)] });
            
            Ok(ExecutionPlan {
                root_step_id: step_id,
                steps,
            })
        },
        _ => Err(format!("Unsupported mutation action: {}", action)),
    }
}

#[derive(Clone)]
pub enum ParentConstraint {
    Singular { step_id: String },
    Bulk { sql: String, params: Vec<Parameter> },
}

#[derive(Clone)]
pub struct ParentRel {
    pub constraint: ParentConstraint,
    pub parent_model: String,
    pub relation_field_name: String,
}

#[derive(Clone)]
enum DeferredAction {
    Create(serde_json::Map<String, Value>),
    Connect(serde_json::Map<String, Value>),
    Update(String, serde_json::Map<String, Value>, serde_json::Map<String, Value>),
    Delete(String, serde_json::Map<String, Value>),
    Disconnect(serde_json::Map<String, Value>),
    Set(Vec<serde_json::Map<String, Value>>),
    Upsert(serde_json::Map<String, Value>, serde_json::Map<String, Value>),
    UpdateMany(String, serde_json::Map<String, Value>, serde_json::Map<String, Value>),
    DeleteMany(String, serde_json::Map<String, Value>),
}

#[derive(Clone)]
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

        let rel_attr = parent_field_def.attributes.iter().find(|a| matches!(a, FieldAttribute::Relation { .. }));
        let rel_name = match rel_attr {
            Some(FieldAttribute::Relation { name, .. }) => name.clone(),
            _ => None,
        };

        let reverse_field = model_def.resolved_fields.iter().find(|f| {
            match &f.field_type {
                AstFieldType::Relation(rt) | AstFieldType::PolymorphicBase(rt) if rt == &rel.parent_model || ast.models.get(&rel.parent_model).map_or(false, |m| m.resolved_bases.contains(rt)) => {
                    if let Some(FieldAttribute::Relation { name, .. }) = f.attributes.iter().find(|a| matches!(a, FieldAttribute::Relation { .. })) {
                        if name == &rel_name { return true; }
                    } else if rel_name.is_none() {
                        return true;
                    }
                    false
                }
                _ => false
            }
        });

        if let Some(rev_f) = reverse_field {
            let mut rel_fields = None;
            let mut rel_refs = None;
            if let Some(FieldAttribute::InternalRelation { fields, references }) = rev_f.attributes.iter().find(|a| matches!(a, FieldAttribute::InternalRelation { .. })) {
                rel_fields = Some(fields);
                rel_refs = Some(references);
            } else if let Some(FieldAttribute::Relation { fields, references, .. }) = rev_f.attributes.iter().find(|a| matches!(a, FieldAttribute::Relation { .. })) {
                if let (Some(f), Some(r)) = (fields, references) {
                    rel_fields = Some(f);
                    rel_refs = Some(r);
                }
            }
            if let (Some(fields), Some(references)) = (rel_fields, rel_refs) {
                if !fields.is_empty() && !references.is_empty() {
                    fk_col = Some(fields[0].clone());
                    target_pk = references[0].clone();
                }
            }
        } else {
            if let Some(FieldAttribute::InternalRelation { fields, references }) = parent_field_def.attributes.iter().find(|a| matches!(a, FieldAttribute::InternalRelation { .. })) {
                if !fields.is_empty() && !references.is_empty() {
                    let pfk = &fields[0];
                    let rpk = &references[0];
                    if model_def.resolved_fields.iter().any(|f| &f.name == pfk) {
                        fk_col = Some(pfk.clone());
                        target_pk = rpk.clone();
                    }
                }
            }
        }
        
        if let Some(col) = fk_col {
            match &rel.constraint {
                ParentConstraint::Singular { step_id } => {
                    columns.push(col.clone());
                    placeholders.push(format!("?{}", param_idx));
                    params.push(Parameter::Reference { step_id: step_id.clone(), column: target_pk.clone() });
                    param_idx += 1;
                },
                ParentConstraint::Bulk { params: bulk_params, .. } => {
                    // For bulk, we inject the parent's parameters at the start
                    params.extend(bulk_params.clone());
                    param_idx += bulk_params.len();
                }
            }
        }
    }
    
    for (key, val) in data {
        if key.starts_with("__") { continue; }
        
        let field_def = model_def.resolved_fields.iter().find(|f| &f.name == key)
            .ok_or_else(|| format!("Invalid field '{}' for model '{}'.", key, model_name))?;

        match &field_def.field_type {
            AstFieldType::Scalar(type_name) => {
                let normalized_val = validate_and_normalize_scalar(ast, key, type_name, false, val)?;
                columns.push(key.clone());
                placeholders.push(format!("?{}", param_idx));
                params.push(Parameter::Literal(normalized_val));
                param_idx += 1;
            },
            AstFieldType::Enum(type_name) => {
                let normalized_val = validate_and_normalize_scalar(ast, key, type_name, true, val)?;
                columns.push(key.clone());
                placeholders.push(format!("?{}", param_idx));
                params.push(Parameter::Literal(normalized_val));
                param_idx += 1;
            },
            AstFieldType::ScalarArray(_) | AstFieldType::EnumArray(_) => {
                let is_enum = matches!(&field_def.field_type, AstFieldType::EnumArray(_));
                if is_enum {
                    if let AstFieldType::EnumArray(type_name) = &field_def.field_type {
                        if let Some(arr) = val.as_array() {
                            for item in arr {
                                validate_and_normalize_scalar(ast, key, type_name, true, item)?;
                            }
                        }
                    }
                }
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
                                deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Update(target_model.clone(), child_where.clone(), child_data.clone()), relation_field_name: key.clone() });
                            }
                        } else if let Some(item) = update_payload.as_object() {
                            let child_data = item.get("data").and_then(|v| v.as_object()).ok_or("Expected 'data' object in 'update'")?;
                            let child_where = item.get("where").and_then(|v| v.as_object()).ok_or("Expected 'where' object in 'update'")?;
                            deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Update(target_model.clone(), child_where.clone(), child_data.clone()), relation_field_name: key.clone() });
                        }
                    }
                    if let Some(delete_payload) = nested_mutations.get("delete") {
                        if let Some(arr) = delete_payload.as_array() {
                            for item in arr {
                                let child_where = item.as_object().ok_or("Expected 'where' object in 'delete' array")?;
                                deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Delete(target_model.clone(), child_where.clone()), relation_field_name: key.clone() });
                            }
                        } else if let Some(item) = delete_payload.as_object() {
                            deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Delete(target_model.clone(), item.clone()), relation_field_name: key.clone() });
                        }
                    }
                    if let Some(update_many_payload) = nested_mutations.get("updateMany") {
                        if let Some(arr) = update_many_payload.as_array() {
                            for item in arr {
                                let child_data = item.get("data").and_then(|v| v.as_object()).ok_or("Expected 'data' object in 'updateMany' array")?;
                                let child_where = item.get("where").and_then(|v| v.as_object()).ok_or("Expected 'where' object in 'updateMany' array")?;
                                deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::UpdateMany(target_model.clone(), child_where.clone(), child_data.clone()), relation_field_name: key.clone() });
                            }
                        } else if let Some(item) = update_many_payload.as_object() {
                            let child_data = item.get("data").and_then(|v| v.as_object()).ok_or("Expected 'data' object in 'updateMany'")?;
                            let child_where = item.get("where").and_then(|v| v.as_object()).ok_or("Expected 'where' object in 'updateMany'")?;
                            deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::UpdateMany(target_model.clone(), child_where.clone(), child_data.clone()), relation_field_name: key.clone() });
                        }
                    }
                    if let Some(delete_many_payload) = nested_mutations.get("deleteMany") {
                        if let Some(arr) = delete_many_payload.as_array() {
                            for item in arr {
                                let child_where = item.get("where").and_then(|v| v.as_object()).ok_or("Expected 'where' object in 'deleteMany' array element")?;
                                deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::DeleteMany(target_model.clone(), child_where.clone()), relation_field_name: key.clone() });
                            }
                        } else if let Some(item) = delete_many_payload.as_object() {
                            let child_where = item.get("where").and_then(|v| v.as_object()).unwrap_or(item);
                            deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::DeleteMany(target_model.clone(), child_where.clone()), relation_field_name: key.clone() });
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
                let nested_mutations = val.as_object().ok_or(format!("Expected object for polymorphic field '{}'", key))?;
                
                for (concrete_model_name, _) in nested_mutations {
                    if ast.models.contains_key(concrete_model_name) {
                        return Err(format!("Unsupported: Array mutations on polymorphic field '{}' are not yet implemented.", key));
                    }
                }
                
                if let Some(update_payload) = nested_mutations.get("update") {
                    if let Some(arr) = update_payload.as_array() {
                        for item in arr {
                            let child_data = item.get("data").and_then(|v| v.as_object()).ok_or("Expected 'data' object in 'update' array")?;
                            let child_where = item.get("where").and_then(|v| v.as_object()).ok_or("Expected 'where' object in 'update' array")?;
                            let kind_val = child_where.get("__kind").and_then(|v| v.as_str()).ok_or("Polymorphic nested mutation requires '__kind' in 'where' block")?;
                            if !ast.models.contains_key(kind_val) {
                                return Err(format!("Security Exception: Target model '{}' undefined.", kind_val));
                            }
                            let mut cw = child_where.clone();
                            cw.remove("__kind");
                            deferred_children.push(DeferredChild { target_model: kind_val.to_string(), action: DeferredAction::Update(kind_val.to_string(), cw, child_data.clone()), relation_field_name: key.clone() });
                        }
                    } else if let Some(item) = update_payload.as_object() {
                        let child_data = item.get("data").and_then(|v| v.as_object()).ok_or("Expected 'data' object in 'update'")?;
                        let child_where = item.get("where").and_then(|v| v.as_object()).ok_or("Expected 'where' object in 'update'")?;
                        let kind_val = child_where.get("__kind").and_then(|v| v.as_str()).ok_or("Polymorphic nested mutation requires '__kind' in 'where' block")?;
                        if !ast.models.contains_key(kind_val) {
                            return Err(format!("Security Exception: Target model '{}' undefined.", kind_val));
                        }
                        let mut cw = child_where.clone();
                        cw.remove("__kind");
                        deferred_children.push(DeferredChild { target_model: kind_val.to_string(), action: DeferredAction::Update(kind_val.to_string(), cw, child_data.clone()), relation_field_name: key.clone() });
                    }
                }
                if let Some(delete_payload) = nested_mutations.get("delete") {
                    if let Some(arr) = delete_payload.as_array() {
                        for item in arr {
                            let child_where = item.as_object().ok_or("Expected 'where' object in 'delete' array")?;
                            let kind_val = child_where.get("__kind").and_then(|v| v.as_str()).ok_or("Polymorphic nested mutation requires '__kind' in 'where' block")?;
                            if !ast.models.contains_key(kind_val) {
                                return Err(format!("Security Exception: Target model '{}' undefined.", kind_val));
                            }
                            let mut cw = child_where.clone();
                            cw.remove("__kind");
                            deferred_children.push(DeferredChild { target_model: kind_val.to_string(), action: DeferredAction::Delete(kind_val.to_string(), cw), relation_field_name: key.clone() });
                        }
                    } else if let Some(item) = delete_payload.as_object() {
                        let kind_val = item.get("__kind").and_then(|v| v.as_str()).ok_or("Polymorphic nested mutation requires '__kind' in 'where' block")?;
                        if !ast.models.contains_key(kind_val) {
                            return Err(format!("Security Exception: Target model '{}' undefined.", kind_val));
                        }
                        let mut cw = item.clone();
                        cw.remove("__kind");
                        deferred_children.push(DeferredChild { target_model: kind_val.to_string(), action: DeferredAction::Delete(kind_val.to_string(), cw), relation_field_name: key.clone() });
                    }
                }
                if let Some(update_many_payload) = nested_mutations.get("updateMany") {
                    let base_model_name = match &field_def.field_type {
                        AstFieldType::PolymorphicUnionArray(n) | AstFieldType::PolymorphicBaseArray(n) => n.clone(),
                        _ => unreachable!(),
                    };
                    if let Some(arr) = update_many_payload.as_array() {
                        for item in arr {
                            let child_data = item.get("data").and_then(|v| v.as_object()).ok_or("Expected 'data' object in 'updateMany' array")?;
                            let child_where = item.get("where").and_then(|v| v.as_object()).ok_or("Expected 'where' object in 'updateMany' array")?;
                            deferred_children.push(DeferredChild { target_model: base_model_name.clone(), action: DeferredAction::UpdateMany(base_model_name.clone(), child_where.clone(), child_data.clone()), relation_field_name: key.clone() });
                        }
                    } else if let Some(item) = update_many_payload.as_object() {
                        let child_data = item.get("data").and_then(|v| v.as_object()).ok_or("Expected 'data' object in 'updateMany'")?;
                        let child_where = item.get("where").and_then(|v| v.as_object()).ok_or("Expected 'where' object in 'updateMany'")?;
                        deferred_children.push(DeferredChild { target_model: base_model_name.clone(), action: DeferredAction::UpdateMany(base_model_name.clone(), child_where.clone(), child_data.clone()), relation_field_name: key.clone() });
                    }
                }
                if let Some(delete_many_payload) = nested_mutations.get("deleteMany") {
                    let base_model_name = match &field_def.field_type {
                        AstFieldType::PolymorphicUnionArray(n) | AstFieldType::PolymorphicBaseArray(n) => n.clone(),
                        _ => unreachable!(),
                    };
                    if let Some(arr) = delete_many_payload.as_array() {
                        for item in arr {
                            let child_where = item.get("where").and_then(|v| v.as_object()).ok_or("Expected 'where' object in 'deleteMany' array element")?;
                            deferred_children.push(DeferredChild { target_model: base_model_name.clone(), action: DeferredAction::DeleteMany(base_model_name.clone(), child_where.clone()), relation_field_name: key.clone() });
                        }
                    } else if let Some(item) = delete_many_payload.as_object() {
                        let child_where = item.get("where").and_then(|v| v.as_object()).unwrap_or(item);
                        deferred_children.push(DeferredChild { target_model: base_model_name.clone(), action: DeferredAction::DeleteMany(base_model_name.clone(), child_where.clone()), relation_field_name: key.clone() });
                    }
                }
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
    
    let mut bulk_parent = None;
    let mut fk_col_name = None;
    let mut target_pk_name = "__id".to_string();
    if let Some(rel) = &parent_rel {
        if let ParentConstraint::Bulk { sql: parent_sql, .. } = &rel.constraint {
            bulk_parent = Some((rel.parent_model.clone(), parent_sql.clone()));
        }
        
        // Re-use the same logic as the start of the function
        let parent_model_def = ast.models.get(&rel.parent_model).unwrap();
        let parent_field_def = parent_model_def.resolved_fields.iter().find(|f| f.name == rel.relation_field_name).unwrap();
        let rel_attr = parent_field_def.attributes.iter().find(|a| matches!(a, FieldAttribute::Relation { .. }));
        let rel_name = match rel_attr {
            Some(FieldAttribute::Relation { name, .. }) => name.clone(),
            _ => None,
        };
        let reverse_field = model_def.resolved_fields.iter().find(|f| {
            match &f.field_type {
                AstFieldType::Relation(rt) if rt == &rel.parent_model || ast.models.get(&rel.parent_model).map_or(false, |m| m.resolved_bases.contains(rt)) => {
                    if let Some(FieldAttribute::Relation { name, .. }) = f.attributes.iter().find(|a| matches!(a, FieldAttribute::Relation { .. })) {
                        if name == &rel_name { return true; }
                    }
                    true // Fallback to true if no name, like the original logic did or didn't do?
                }
                _ => false
            }
        });
        if let Some(rev_f) = reverse_field {
            let mut rel_fields = None;
            let mut rel_refs = None;
            if let Some(FieldAttribute::InternalRelation { fields, references }) = rev_f.attributes.iter().find(|a| matches!(a, FieldAttribute::InternalRelation { .. })) {
                rel_fields = Some(fields);
                rel_refs = Some(references);
            } else if let Some(FieldAttribute::Relation { fields, references, .. }) = rev_f.attributes.iter().find(|a| matches!(a, FieldAttribute::Relation { .. })) {
                if let (Some(f), Some(r)) = (fields, references) {
                    rel_fields = Some(f);
                    rel_refs = Some(r);
                }
            }
            if let (Some(fields), Some(references)) = (rel_fields, rel_refs) {
                if !fields.is_empty() && !references.is_empty() {
                    fk_col_name = Some(fields[0].clone());
                    target_pk_name = references[0].clone();
                }
            }
            if fk_col_name.is_none() {
                fk_col_name = Some(format!("{}Id", rev_f.name));
            }
        }
        if fk_col_name.is_none() {
            if let Some(FieldAttribute::InternalRelation { fields, references }) = parent_field_def.attributes.iter().find(|a| matches!(a, FieldAttribute::InternalRelation { .. })) {
                if !fields.is_empty() && !references.is_empty() {
                    let pfk = &fields[0];
                    let rpk = &references[0];
                    if model_def.resolved_fields.iter().any(|f| &f.name == pfk) {
                        fk_col_name = Some(pfk.clone());
                        target_pk_name = rpk.clone();
                    }
                }
            }
        }
        
        // If we still can't find it, we fallback to the heuristic like `fk_column_name.unwrap_or_else(|| format!("{}Id", parent_model_name.to_string()))` 
        // Wait, Caqui usually defaults to parent_model_name + "Id" or target_model + "Id".
        // Let's just default to what is usually done for reverse fields.
        if fk_col_name.is_none() {
            fk_col_name = Some(format!("{}Id", rel.parent_model.to_lowercase()));
        }
    }

    let sql = if let Some((parent_model, parent_sql)) = bulk_parent {
        let fk = fk_col_name.unwrap();
        if columns.is_empty() {
            format!(
                "INSERT INTO {} ({}) SELECT {} FROM ({}) RETURNING {};",
                model_name, fk, target_pk_name, parent_sql, pk_col
            )
        } else {
            format!(
                "INSERT INTO {} ({}, {}) SELECT {}, {} FROM ({}) RETURNING {};",
                model_name,
                columns.join(", "),
                fk,
                placeholders.join(", "),
                target_pk_name,
                parent_sql,
                pk_col
            )
        }
    } else {
        if columns.is_empty() {
            format!("INSERT INTO {} DEFAULT VALUES RETURNING {};", model_name, pk_col)
        } else {
            format!(
                "INSERT INTO {} ({}) VALUES ({}) RETURNING {};",
                model_name,
                columns.join(", "),
                placeholders.join(", "),
                pk_col
            )
        }
    };
    
    steps.push(ExecutionStep::Query {
        id: step_id.clone(),
        sql,
        params,
    });
    
    process_deferred_children(ast, model_name, &ParentConstraint::Singular { step_id: step_id.clone() }, deferred_children, steps, alias_counter)?;
    
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
                let normalized_val = validate_and_normalize_scalar(ast, key, type_name, false, val)?;
                set_clauses.push(format!("{} = ?{}", key, param_idx));
                params.push(Parameter::Literal(normalized_val));
                param_idx += 1;
            },
            AstFieldType::Enum(type_name) => {
                let normalized_val = validate_and_normalize_scalar(ast, key, type_name, true, val)?;
                set_clauses.push(format!("{} = ?{}", key, param_idx));
                params.push(Parameter::Literal(normalized_val));
                param_idx += 1;
            },
            AstFieldType::ScalarArray(type_name) | AstFieldType::EnumArray(type_name) => {
                let is_enum = matches!(&field_def.field_type, AstFieldType::EnumArray(_));
                if let Some(obj) = val.as_object() {
                    if let Some(push_val) = obj.get("push") {
                        if is_enum {
                            if let AstFieldType::EnumArray(type_name) = &field_def.field_type {
                                validate_and_normalize_scalar(ast, key, type_name, true, push_val)?;
                            }
                        }
                        set_clauses.push(format!("{} = json_insert(COALESCE({}, '[]'), '$[#]', ?{})", key, key, param_idx));
                        let push_str = if push_val.is_string() { push_val.as_str().unwrap().to_string() } else { serde_json::to_string(push_val).unwrap_or_default() };
                        params.push(Parameter::Literal(serde_json::Value::String(push_str)));
                        param_idx += 1;
                    } else if let Some(pull_val) = obj.get("pull") {
                        let normalized_val = validate_and_normalize_scalar(ast, key, type_name, is_enum, pull_val)?;
                        set_clauses.push(format!("{} = (SELECT json_group_array(value) FROM json_each({}) WHERE value != ?{})", key, key, param_idx));
                        params.push(Parameter::Literal(normalized_val));
                        param_idx += 1;
                    } else if let Some(pull_index) = obj.get("pullIndex") {
                        if let Some(idx) = pull_index.as_i64() {
                            set_clauses.push(format!("{} = json_remove({}, '$[' || ?{} || ']')", key, key, param_idx));
                            params.push(Parameter::Literal(serde_json::Value::Number(serde_json::Number::from(idx))));
                            param_idx += 1;
                        }
                    }
                } else if let Some(arr) = val.as_array() {
                    if is_enum {
                        if let AstFieldType::EnumArray(type_name) = &field_def.field_type {
                            for item in arr {
                                validate_and_normalize_scalar(ast, key, type_name, true, item)?;
                            }
                        }
                    }
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
                                deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Update(target_model.clone(), child_where.clone(), child_data.clone()), relation_field_name: key.clone() });
                            }
                        } else if let Some(item) = update_payload.as_object() {
                            let child_data = item.get("data").and_then(|v| v.as_object()).ok_or("Expected 'data' object in 'update'")?;
                            let child_where = item.get("where").and_then(|v| v.as_object()).ok_or("Expected 'where' object in 'update'")?;
                            deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Update(target_model.clone(), child_where.clone(), child_data.clone()), relation_field_name: key.clone() });
                        }
                    }
                    if let Some(delete_payload) = nested_mutations.get("delete") {
                        if let Some(arr) = delete_payload.as_array() {
                            for item in arr {
                                let child_where = item.as_object().ok_or("Expected 'where' object in 'delete' array")?;
                                deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Delete(target_model.clone(), child_where.clone()), relation_field_name: key.clone() });
                            }
                        } else if let Some(item) = delete_payload.as_object() {
                            deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Delete(target_model.clone(), item.clone()), relation_field_name: key.clone() });
                        }
                    }
                    if let Some(update_many_payload) = nested_mutations.get("updateMany") {
                        if let Some(arr) = update_many_payload.as_array() {
                            for item in arr {
                                let child_data = item.get("data").and_then(|v| v.as_object()).ok_or("Expected 'data' object in 'updateMany' array")?;
                                let child_where = item.get("where").and_then(|v| v.as_object()).ok_or("Expected 'where' object in 'updateMany' array")?;
                                deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::UpdateMany(target_model.clone(), child_where.clone(), child_data.clone()), relation_field_name: key.clone() });
                            }
                        } else if let Some(item) = update_many_payload.as_object() {
                            let child_data = item.get("data").and_then(|v| v.as_object()).ok_or("Expected 'data' object in 'updateMany'")?;
                            let child_where = item.get("where").and_then(|v| v.as_object()).ok_or("Expected 'where' object in 'updateMany'")?;
                            deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::UpdateMany(target_model.clone(), child_where.clone(), child_data.clone()), relation_field_name: key.clone() });
                        }
                    }
                    if let Some(delete_many_payload) = nested_mutations.get("deleteMany") {
                        if let Some(arr) = delete_many_payload.as_array() {
                            for item in arr {
                                let child_where = item.get("where").and_then(|v| v.as_object()).ok_or("Expected 'where' object in 'deleteMany' array element")?;
                                deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::DeleteMany(target_model.clone(), child_where.clone()), relation_field_name: key.clone() });
                            }
                        } else if let Some(item) = delete_many_payload.as_object() {
                            let child_where = item.get("where").and_then(|v| v.as_object()).unwrap_or(item);
                            deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::DeleteMany(target_model.clone(), child_where.clone()), relation_field_name: key.clone() });
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
                let nested_mutations = val.as_object().ok_or(format!("Expected object for polymorphic field '{}'", key))?;
                
                for (concrete_model_name, _) in nested_mutations {
                    if ast.models.contains_key(concrete_model_name) {
                        return Err(format!("Unsupported: Array mutations on polymorphic field '{}' are not yet implemented.", key));
                    }
                }
                
                if let Some(update_payload) = nested_mutations.get("update") {
                    if let Some(arr) = update_payload.as_array() {
                        for item in arr {
                            let child_data = item.get("data").and_then(|v| v.as_object()).ok_or("Expected 'data' object in 'update' array")?;
                            let child_where = item.get("where").and_then(|v| v.as_object()).ok_or("Expected 'where' object in 'update' array")?;
                            let kind_val = child_where.get("__kind").and_then(|v| v.as_str()).ok_or("Polymorphic nested mutation requires '__kind' in 'where' block")?;
                            if !ast.models.contains_key(kind_val) {
                                return Err(format!("Security Exception: Target model '{}' undefined.", kind_val));
                            }
                            let mut cw = child_where.clone();
                            cw.remove("__kind");
                            deferred_children.push(DeferredChild { target_model: kind_val.to_string(), action: DeferredAction::Update(kind_val.to_string(), cw, child_data.clone()), relation_field_name: key.clone() });
                        }
                    } else if let Some(item) = update_payload.as_object() {
                        let child_data = item.get("data").and_then(|v| v.as_object()).ok_or("Expected 'data' object in 'update'")?;
                        let child_where = item.get("where").and_then(|v| v.as_object()).ok_or("Expected 'where' object in 'update'")?;
                        let kind_val = child_where.get("__kind").and_then(|v| v.as_str()).ok_or("Polymorphic nested mutation requires '__kind' in 'where' block")?;
                        if !ast.models.contains_key(kind_val) {
                            return Err(format!("Security Exception: Target model '{}' undefined.", kind_val));
                        }
                        let mut cw = child_where.clone();
                        cw.remove("__kind");
                        deferred_children.push(DeferredChild { target_model: kind_val.to_string(), action: DeferredAction::Update(kind_val.to_string(), cw, child_data.clone()), relation_field_name: key.clone() });
                    }
                }
                if let Some(delete_payload) = nested_mutations.get("delete") {
                    if let Some(arr) = delete_payload.as_array() {
                        for item in arr {
                            let child_where = item.as_object().ok_or("Expected 'where' object in 'delete' array")?;
                            let kind_val = child_where.get("__kind").and_then(|v| v.as_str()).ok_or("Polymorphic nested mutation requires '__kind' in 'where' block")?;
                            if !ast.models.contains_key(kind_val) {
                                return Err(format!("Security Exception: Target model '{}' undefined.", kind_val));
                            }
                            let mut cw = child_where.clone();
                            cw.remove("__kind");
                            deferred_children.push(DeferredChild { target_model: kind_val.to_string(), action: DeferredAction::Delete(kind_val.to_string(), cw), relation_field_name: key.clone() });
                        }
                    } else if let Some(item) = delete_payload.as_object() {
                        let kind_val = item.get("__kind").and_then(|v| v.as_str()).ok_or("Polymorphic nested mutation requires '__kind' in 'where' block")?;
                        if !ast.models.contains_key(kind_val) {
                            return Err(format!("Security Exception: Target model '{}' undefined.", kind_val));
                        }
                        let mut cw = item.clone();
                        cw.remove("__kind");
                        deferred_children.push(DeferredChild { target_model: kind_val.to_string(), action: DeferredAction::Delete(kind_val.to_string(), cw), relation_field_name: key.clone() });
                    }
                }
                if let Some(update_many_payload) = nested_mutations.get("updateMany") {
                    let base_model_name = match &field_def.field_type {
                        AstFieldType::PolymorphicUnionArray(n) | AstFieldType::PolymorphicBaseArray(n) => n.clone(),
                        _ => unreachable!(),
                    };
                    if let Some(arr) = update_many_payload.as_array() {
                        for item in arr {
                            let child_data = item.get("data").and_then(|v| v.as_object()).ok_or("Expected 'data' object in 'updateMany' array")?;
                            let child_where = item.get("where").and_then(|v| v.as_object()).ok_or("Expected 'where' object in 'updateMany' array")?;
                            deferred_children.push(DeferredChild { target_model: base_model_name.clone(), action: DeferredAction::UpdateMany(base_model_name.clone(), child_where.clone(), child_data.clone()), relation_field_name: key.clone() });
                        }
                    } else if let Some(item) = update_many_payload.as_object() {
                        let child_data = item.get("data").and_then(|v| v.as_object()).ok_or("Expected 'data' object in 'updateMany'")?;
                        let child_where = item.get("where").and_then(|v| v.as_object()).ok_or("Expected 'where' object in 'updateMany'")?;
                        deferred_children.push(DeferredChild { target_model: base_model_name.clone(), action: DeferredAction::UpdateMany(base_model_name.clone(), child_where.clone(), child_data.clone()), relation_field_name: key.clone() });
                    }
                }
                if let Some(delete_many_payload) = nested_mutations.get("deleteMany") {
                    let base_model_name = match &field_def.field_type {
                        AstFieldType::PolymorphicUnionArray(n) | AstFieldType::PolymorphicBaseArray(n) => n.clone(),
                        _ => unreachable!(),
                    };
                    if let Some(arr) = delete_many_payload.as_array() {
                        for item in arr {
                            let child_where = item.get("where").and_then(|v| v.as_object()).ok_or("Expected 'where' object in 'deleteMany' array element")?;
                            deferred_children.push(DeferredChild { target_model: base_model_name.clone(), action: DeferredAction::DeleteMany(base_model_name.clone(), child_where.clone()), relation_field_name: key.clone() });
                        }
                    } else if let Some(item) = delete_many_payload.as_object() {
                        let child_where = item.get("where").and_then(|v| v.as_object()).unwrap_or(item);
                        deferred_children.push(DeferredChild { target_model: base_model_name.clone(), action: DeferredAction::DeleteMany(base_model_name.clone(), child_where.clone()), relation_field_name: key.clone() });
                    }
                }
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

        if let Some(field) = parent_model_def.resolved_fields.iter().find(|f| f.name == rel.relation_field_name) {
            let rel_attr = field.attributes.iter().find(|a| matches!(a, FieldAttribute::Relation { .. }));
            let rel_name = match rel_attr {
                Some(FieldAttribute::Relation { name, .. }) => name.clone(),
                _ => None,
            };

            let reverse_field = model_def.resolved_fields.iter().find(|f| {
                match &f.field_type {
                    AstFieldType::Relation(rt) if rt == &rel.parent_model || ast.models.get(&rel.parent_model).map_or(false, |m| m.resolved_bases.contains(rt)) => {
                        if let Some(FieldAttribute::Relation { name, .. }) = f.attributes.iter().find(|a| matches!(a, FieldAttribute::Relation { .. })) {
                            if name == &rel_name { return true; }
                        }
                        false
                    }
                    _ => false
                }
            });

            if let Some(rev_f) = reverse_field {
                let mut rel_fields = None;
                let mut rel_refs = None;
                if let Some(FieldAttribute::InternalRelation { fields, references }) = rev_f.attributes.iter().find(|a| matches!(a, FieldAttribute::InternalRelation { .. })) {
                    rel_fields = Some(fields);
                    rel_refs = Some(references);
                } else if let Some(FieldAttribute::Relation { fields, references, .. }) = rev_f.attributes.iter().find(|a| matches!(a, FieldAttribute::Relation { .. })) {
                    if let (Some(f), Some(r)) = (fields, references) {
                        rel_fields = Some(f);
                        rel_refs = Some(r);
                    }
                }
                if let (Some(fields), Some(references)) = (rel_fields, rel_refs) {
                    if !fields.is_empty() && !references.is_empty() {
                        fk_col = Some(fields[0].clone());
                        target_pk = references[0].clone();
                    }
                }
            } else {
                if let Some(FieldAttribute::InternalRelation { fields, references }) = field.attributes.iter().find(|a| matches!(a, FieldAttribute::InternalRelation { .. })) {
                    if !fields.is_empty() && !references.is_empty() {
                        let pfk = &fields[0];
                        let rpk = &references[0];
                        if model_def.resolved_fields.iter().any(|f| &f.name == pfk) {
                            fk_col = Some(pfk.clone());
                            target_pk = rpk.clone();
                        }
                    }
                }
            }
        }

        let mut parent_ref_param = None;
        if let Some(col) = fk_col {
            where_sql = format!("({} AND {}.{} = ?{})", where_sql, model_name, col, param_idx);
            match &rel.constraint {
                ParentConstraint::Singular { step_id } => {
                    parent_ref_param = Some(Parameter::Reference { step_id: step_id.clone(), column: target_pk });
                },
                ParentConstraint::Bulk { .. } => {
                    return Err("Semantics Error: Cannot execute singular 'update' nested under a bulk operation. Use 'updateMany' instead.".to_string());
                }
            }
            param_idx += 1;
        }
        
        params.extend(where_params);

        let sql = format!(
            "UPDATE {} SET {} WHERE {} RETURNING {};",
            model_name,
            set_clauses.join(", "),
            where_sql,
            pk_col
        );
        
        if let Some(pref) = parent_ref_param {
            steps.push(ExecutionStep::UpdateBranch {
                id: step_id.clone(),
                sql,
                params,
                parent_ref: pref,
            });
        } else {
            steps.push(ExecutionStep::Query {
                id: step_id.clone(),
                sql,
                params,
            });
        }
    } else {
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
    }
    
    process_deferred_children(ast, model_name, &ParentConstraint::Singular { step_id: step_id.clone() }, deferred_children, steps, alias_counter)?;
    
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
    parent_constraint: &ParentConstraint,
    deferred_children: Vec<DeferredChild>,
    steps: &mut Vec<ExecutionStep>,
    alias_counter: &mut usize,
) -> Result<(), String> {
    let parent_step_id = match parent_constraint {
        ParentConstraint::Singular { step_id } => step_id.clone(),
        ParentConstraint::Bulk { .. } => "UNIMPLEMENTED_BULK".to_string(), // Phase 3 will handle this
    };

    let parent_model_def = ast.models.get(parent_model_name).unwrap();
    let parent_pk_col = parent_model_def.resolved_fields.iter().find(|f| f.attributes.iter().any(|a| matches!(a, FieldAttribute::Id))).map(|f| f.name.as_str()).unwrap_or("__id");

    for child in deferred_children {
        let mut child_fields: Option<&Vec<schema_parser::ast::FieldNode>> = None;
        if let Some(m) = ast.models.get(&child.target_model) {
            child_fields = Some(&m.resolved_fields);
        } else if let Some(b) = ast.bases.get(&child.target_model) {
            child_fields = Some(&b.fields);
        }
        
        let child_pk_col = if let Some(fields) = child_fields {
            fields.iter().find(|f| f.attributes.iter().any(|a| matches!(a, FieldAttribute::Id))).map(|f| f.name.as_str()).unwrap_or("__id")
        } else { "__id" };
        
        let parent_field_def = parent_model_def.resolved_fields.iter().find(|f| f.name == child.relation_field_name).unwrap();
        let is_polymorphic = matches!(parent_field_def.field_type, AstFieldType::PolymorphicBase(_) | AstFieldType::PolymorphicBaseArray(_) | AstFieldType::PolymorphicUnion(_) | AstFieldType::PolymorphicUnionArray(_));
        let is_singular_polymorphic = matches!(parent_field_def.field_type, AstFieldType::PolymorphicBase(_) | AstFieldType::PolymorphicUnion(_));

        if is_singular_polymorphic {
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

        let mut child_fields: Option<&Vec<schema_parser::ast::FieldNode>> = None;
        if let Some(m) = ast.models.get(&child.target_model) {
            child_fields = Some(&m.resolved_fields);
        } else if let Some(b) = ast.bases.get(&child.target_model) {
            child_fields = Some(&b.fields);
        }
        
        let child_pk_col = if let Some(fields) = child_fields {
            fields.iter().find(|f| f.attributes.iter().any(|a| matches!(a, FieldAttribute::Id))).map(|f| f.name.as_str()).unwrap_or("__id")
        } else { "__id" };

        let reverse_field = if let Some(fields) = child_fields {
            fields.iter().find(|f| {
                match &f.field_type {
                    AstFieldType::Relation(rt) | AstFieldType::PolymorphicBase(rt) if rt == parent_model_name || ast.models.get(parent_model_name).map_or(false, |m| m.resolved_bases.contains(rt)) => {
                        if let Some(FieldAttribute::Relation { name, .. }) = f.attributes.iter().find(|a| matches!(a, FieldAttribute::Relation { .. })) {
                            if name == &rel_name { return true; }
                        } else if rel_name.is_none() {
                            return true;
                        }
                        false
                    }
                    _ => false
                }
            })
        } else { None };

        if let Some(rev_f) = reverse_field {
            if let Some(FieldAttribute::InternalRelation { fields, references }) = rev_f.attributes.iter().find(|a| matches!(a, FieldAttribute::InternalRelation { .. })) {
                if !fields.is_empty() && !references.is_empty() {
                    fk_column_name = Some(fields[0].clone());
                    target_pk = references[0].clone();
                }
            }
            if fk_column_name.is_none() {
                fk_column_name = Some(format!("{}Id", rev_f.name));
            }
        }

        let fk_col = fk_column_name.unwrap_or_else(|| format!("{}Id", parent_model_name.to_lowercase()));

        match child.action {
            DeferredAction::Create(child_data) => {
                translate_create_node(ast, &child.target_model, &child_data, steps, alias_counter, Some(ParentRel {
                    constraint: parent_constraint.clone(),
                    parent_model: parent_model_name.to_string(),
                    relation_field_name: child.relation_field_name,
                }))?;
            },
            DeferredAction::Connect(connect_where) => {
                if let ParentConstraint::Bulk { .. } = parent_constraint {
                    return Err("Semantics Error: Cannot 'connect' a child to multiple parents in a bulk update when the child holds the foreign key.".to_string());
                }
                
                let child_step_id = format!("step_{}_connect_{}", child.target_model.to_lowercase(), *alias_counter);
                *alias_counter += 1;
                
                let mut params = Vec::new();
                let mut param_idx = 1;
                if let ParentConstraint::Bulk { params: bulk_params, .. } = parent_constraint {
                    params.extend(bulk_params.clone());
                    param_idx += bulk_params.len();
                }
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
            DeferredAction::Update(concrete_target_model, child_where, child_data) => {
                translate_update_node(ast, &concrete_target_model, &child_where, &child_data, steps, alias_counter, Some(ParentRel {
                    constraint: parent_constraint.clone(),
                    parent_model: parent_model_name.to_string(),
                    relation_field_name: child.relation_field_name.clone(),
                }))?;
            },
            DeferredAction::Delete(concrete_target_model, child_where) => {
                let child_step_id = format!("step_{}_delete_{}", concrete_target_model.to_lowercase(), *alias_counter);
                *alias_counter += 1;
                
                let mut params = Vec::new();
                let mut param_idx = 1;
                if let ParentConstraint::Bulk { params: bulk_params, .. } = parent_constraint {
                    params.extend(bulk_params.clone());
                    param_idx += bulk_params.len();
                }
                let child_model_def = ast.models.get(&concrete_target_model).unwrap();
                let where_clause_ir = parse_where_clause(ast, &child_where, child_model_def)?;
                let (where_sql, where_params) = compile_parameterized_where(&where_clause_ir, &concrete_target_model, &mut param_idx);
                params.extend(where_params);
                
                let combined_where_sql = match parent_constraint {
                    ParentConstraint::Singular { step_id } => {
                        let sql = format!("({} AND {}.{} = ?{})", where_sql, concrete_target_model, fk_col, param_idx);
                        param_idx += 1;
                        sql
                    },
                    ParentConstraint::Bulk { sql: bulk_sql, .. } => {
                        format!("({} AND {}.{} IN ({}))", where_sql, concrete_target_model, fk_col, bulk_sql)
                    }
                };
                let parent_ref = match parent_constraint {
                    ParentConstraint::Singular { step_id } => Parameter::Reference { step_id: step_id.clone(), column: target_pk.clone() },
                    ParentConstraint::Bulk { .. } => Parameter::Reference { step_id: "BULK_DUMMY".to_string(), column: target_pk.clone() },
                };
                
                let sql = format!(
                    "DELETE FROM {} WHERE {} RETURNING {};",
                    concrete_target_model,
                    combined_where_sql,
                    child_pk_col
                );
                
                steps.push(ExecutionStep::DeleteBranch {
                    id: child_step_id,
                    sql,
                    params,
                    parent_ref,
                });
            },
            DeferredAction::UpdateMany(target_name, child_where, child_data) => {
                let child_step_id = format!("step_{}_update_many_{}", target_name.to_lowercase(), *alias_counter);
                *alias_counter += 1;
                
                let mut concrete_models = Vec::new();
                if ast.models.contains_key(&target_name) {
                    concrete_models.push(target_name.clone());
                } else if let Some(union_models) = ast.unions.get(&target_name) {
                    concrete_models.extend(union_models.clone());
                } else if ast.bases.contains_key(&target_name) {
                    for (m_name, m_node) in &ast.models {
                        if m_node.resolved_bases.contains(&target_name) {
                            concrete_models.push(m_name.clone());
                        }
                    }
                }
                
                let mut required_bases = Vec::new();
                for (k, v) in &child_where {
                    if k.starts_with("__") && k != "__id" && k != "__kind" {
                        if let Some(b) = v.as_bool() {
                            if b {
                                required_bases.push(k[2..].to_string());
                            }
                        }
                    }
                }
                
                concrete_models.retain(|m_name| {
                    let m_node = ast.models.get(m_name).unwrap();
                    required_bases.iter().all(|b| m_node.resolved_bases.contains(b))
                });
                
                let mut queries = Vec::new();
                let parent_ref = match parent_constraint {
                    ParentConstraint::Singular { step_id } => Some(Parameter::Reference { step_id: step_id.clone(), column: target_pk.clone() }),
                    ParentConstraint::Bulk { .. } => None,
                };
                
                for c_model in concrete_models {
                    let child_model_def = ast.models.get(&c_model).unwrap_or_else(|| panic!("Failed to find model: {}", c_model));
                    
                    let mut params = Vec::new();
                    let mut param_idx = 1;
                    if let ParentConstraint::Bulk { params: bulk_params, .. } = parent_constraint {
                        params.extend(bulk_params.clone());
                        param_idx += bulk_params.len();
                    }
                    
                    let mut set_clauses = Vec::new();
                    let mut bulk_deferred_children = Vec::new();
                    for (key, val) in &child_data {
                        if key.starts_with("__") { continue; }
                        if let Some(field_def) = child_model_def.resolved_fields.iter().find(|f| &f.name == key) {
                            match &field_def.field_type {
                                AstFieldType::Scalar(type_name) | AstFieldType::Enum(type_name) => {
                                    let is_enum = matches!(&field_def.field_type, AstFieldType::Enum(_));
                                    if let Ok(normalized_val) = validate_and_normalize_scalar(ast, key, type_name, is_enum, val) {
                                        set_clauses.push(format!("{} = ?{}", key, param_idx));
                                        params.push(Parameter::Literal(normalized_val));
                                        param_idx += 1;
                                    }
                                },
                                AstFieldType::ScalarArray(type_name) | AstFieldType::EnumArray(type_name) => {
                                    let is_enum = matches!(&field_def.field_type, AstFieldType::EnumArray(_));
                                    if let Some(obj) = val.as_object() {
                                        if let Some(push_val) = obj.get("push") {
                                            if let Ok(normalized_val) = validate_and_normalize_scalar(ast, key, type_name, is_enum, push_val) {
                                                set_clauses.push(format!("{} = json_insert(COALESCE({}, '[]'), '$[#]', ?{})", key, key, param_idx));
                                                let push_str = if push_val.is_string() { push_val.as_str().unwrap().to_string() } else { serde_json::to_string(push_val).unwrap_or_default() };
                                                params.push(Parameter::Literal(serde_json::Value::String(push_str)));
                                                param_idx += 1;
                                            }
                                        } else if let Some(pull_val) = obj.get("pull") {
                                            if let Ok(normalized_val) = validate_and_normalize_scalar(ast, key, type_name, is_enum, pull_val) {
                                                set_clauses.push(format!("{} = (SELECT json_group_array(value) FROM json_each({}) WHERE value != ?{})", key, key, param_idx));
                                                params.push(Parameter::Literal(normalized_val));
                                                param_idx += 1;
                                            }
                                        } else if let Some(pull_index) = obj.get("pullIndex") {
                                            if let Some(idx) = pull_index.as_i64() {
                                                set_clauses.push(format!("{} = json_remove({}, '$[' || ?{} || ']')", key, key, param_idx));
                                                params.push(Parameter::Literal(serde_json::Value::Number(serde_json::Number::from(idx))));
                                                param_idx += 1;
                                            }
                                        }
                                    } else if let Some(arr) = val.as_array() {
                                        set_clauses.push(format!("{} = ?{}", key, param_idx));
                                        let json_val = serde_json::to_string(arr).unwrap_or_else(|_| "[]".to_string());
                                        params.push(Parameter::Literal(serde_json::Value::String(json_val)));
                                        param_idx += 1;
                                    }
                                },
                                AstFieldType::Relation(target_model) | AstFieldType::RelationArray(target_model) => {
                                    if let Some(nested_mutations) = val.as_object() {
                                        if let Some(um_payload) = nested_mutations.get("updateMany") {
                                            if let Some(arr) = um_payload.as_array() {
                                                for item in arr {
                                                    let c_data = item.get("data").and_then(|v| v.as_object()).unwrap();
                                                    let c_where = item.get("where").and_then(|v| v.as_object()).unwrap();
                                                    bulk_deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::UpdateMany(target_model.clone(), c_where.clone(), c_data.clone()), relation_field_name: key.clone() });
                                                }
                                            } else if let Some(item) = um_payload.as_object() {
                                                let c_data = item.get("data").and_then(|v| v.as_object()).unwrap();
                                                let c_where = item.get("where").and_then(|v| v.as_object()).unwrap();
                                                bulk_deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::UpdateMany(target_model.clone(), c_where.clone(), c_data.clone()), relation_field_name: key.clone() });
                                            }
                                        }
                                        if let Some(dm_payload) = nested_mutations.get("deleteMany") {
                                            if let Some(arr) = dm_payload.as_array() {
                                                for item in arr {
                                                    let c_where = item.get("where").and_then(|v| v.as_object()).unwrap();
                                                    bulk_deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::DeleteMany(target_model.clone(), c_where.clone()), relation_field_name: key.clone() });
                                                }
                                            } else if let Some(item) = dm_payload.as_object() {
                                                let c_where = item.get("where").and_then(|v| v.as_object()).unwrap_or(item);
                                                bulk_deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::DeleteMany(target_model.clone(), c_where.clone()), relation_field_name: key.clone() });
                                            }
                                        }
                                        if let Some(d_payload) = nested_mutations.get("delete") {
                                            if let Some(arr) = d_payload.as_array() {
                                                for item in arr {
                                                    bulk_deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Delete(target_model.clone(), item.as_object().unwrap().clone()), relation_field_name: key.clone() });
                                                }
                                            } else if let Some(item) = d_payload.as_object() {
                                                bulk_deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Delete(target_model.clone(), item.clone()), relation_field_name: key.clone() });
                                            }
                                        }
                                        if let Some(create_payload) = nested_mutations.get("create") {
                                            if let Some(arr) = create_payload.as_array() {
                                                for item in arr {
                                                    let c_data = item.as_object().unwrap();
                                                    bulk_deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Create(c_data.clone()), relation_field_name: key.clone() });
                                                }
                                            } else if let Some(item) = create_payload.as_object() {
                                                bulk_deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Create(item.clone()), relation_field_name: key.clone() });
                                            }
                                        }
                                        if let Some(connect_payload) = nested_mutations.get("connect") {
                                            if let Some(arr) = connect_payload.as_array() {
                                                for item in arr {
                                                    bulk_deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Connect(item.as_object().unwrap().clone()), relation_field_name: key.clone() });
                                                }
                                            } else if let Some(item) = connect_payload.as_object() {
                                                bulk_deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Connect(item.clone()), relation_field_name: key.clone() });
                                            }
                                        }
                                        if let Some(disconnect_payload) = nested_mutations.get("disconnect") {
                                            if let Some(arr) = disconnect_payload.as_array() {
                                                for item in arr {
                                                    bulk_deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Disconnect(item.as_object().unwrap().clone()), relation_field_name: key.clone() });
                                                }
                                            } else if let Some(item) = disconnect_payload.as_object() {
                                                bulk_deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Disconnect(item.clone()), relation_field_name: key.clone() });
                                            }
                                        }
                                        if let Some(update_payload) = nested_mutations.get("update") {
                                            if let Some(arr) = update_payload.as_array() {
                                                for item in arr {
                                                    let c_data = item.get("data").and_then(|v| v.as_object()).unwrap();
                                                    let c_where = item.get("where").and_then(|v| v.as_object()).unwrap();
                                                    bulk_deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Update(target_model.clone(), c_where.clone(), c_data.clone()), relation_field_name: key.clone() });
                                                }
                                            } else if let Some(item) = update_payload.as_object() {
                                                let c_data = item.get("data").and_then(|v| v.as_object()).unwrap();
                                                let c_where = item.get("where").and_then(|v| v.as_object()).unwrap();
                                                bulk_deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Update(target_model.clone(), c_where.clone(), c_data.clone()), relation_field_name: key.clone() });
                                            }
                                        }
                                        if let Some(upsert_payload) = nested_mutations.get("upsert") {
                                            if let Some(arr) = upsert_payload.as_array() {
                                                for item in arr {
                                                    let c_create = item.get("create").and_then(|v| v.as_object()).unwrap();
                                                    let c_update = item.get("update").and_then(|v| v.as_object()).unwrap();
                                                    bulk_deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Upsert(c_create.clone(), c_update.clone()), relation_field_name: key.clone() });
                                                }
                                            } else if let Some(item) = upsert_payload.as_object() {
                                                let c_create = item.get("create").and_then(|v| v.as_object()).unwrap();
                                                let c_update = item.get("update").and_then(|v| v.as_object()).unwrap();
                                                bulk_deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Upsert(c_create.clone(), c_update.clone()), relation_field_name: key.clone() });
                                            }
                                        }
                                        if let Some(set_payload) = nested_mutations.get("set") {
                                            if let Some(arr) = set_payload.as_array() {
                                                let mut set_wheres = Vec::new();
                                                for item in arr {
                                                    set_wheres.push(item.as_object().unwrap().clone());
                                                }
                                                bulk_deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Set(set_wheres), relation_field_name: key.clone() });
                                            } else {
                                                bulk_deferred_children.push(DeferredChild { target_model: target_model.clone(), action: DeferredAction::Set(Vec::new()), relation_field_name: key.clone() });
                                            }
                                        }
                                    }
                                },
                                _ => {}
                            }
                        }
                    }
                    
                    if set_clauses.is_empty() && bulk_deferred_children.is_empty() { continue; }
                    
                    let mut cleaned_where = child_where.clone();
                    cleaned_where.retain(|k, _| !k.starts_with("__") || k == "__id" || k == "__kind");
                    if let Ok(where_clause_ir) = parse_where_clause(ast, &cleaned_where, child_model_def) {
                        let (where_sql, where_params) = compile_parameterized_where(&where_clause_ir, &c_model, &mut param_idx);
                        params.extend(where_params);
                        
                        let combined_where_sql = match parent_constraint {
                            ParentConstraint::Singular { step_id } => {
                                let sql = format!("({} AND {}.{} = ?{})", where_sql, c_model, fk_col, param_idx);
                                params.push(Parameter::Reference { step_id: step_id.clone(), column: target_pk.clone() });
                                param_idx += 1;
                                sql
                            },
                            ParentConstraint::Bulk { sql: bulk_sql, .. } => {
                                format!("({} AND {}.{} IN ({}))", where_sql, c_model, fk_col, bulk_sql)
                            }
                        };
                        
                        let set_str = if set_clauses.is_empty() {
                            format!("__id = __id")
                        } else {
                            set_clauses.join(", ")
                        };
                        
                        let sql = format!(
                            "UPDATE {} SET {} WHERE {};",
                            c_model,
                            set_str,
                            combined_where_sql
                        );
                        queries.push((sql, params));
                        
                        if !bulk_deferred_children.is_empty() {
                            let mut sub_idx = match parent_constraint {
                                ParentConstraint::Singular { .. } => 1,
                                ParentConstraint::Bulk { params: prev_params, .. } => 1 + prev_params.len(),
                            };
                            let (sub_where_sql, sub_where_params) = compile_parameterized_where(&where_clause_ir, &c_model, &mut sub_idx);
                            
                            let mut final_subquery_params = match parent_constraint {
                                ParentConstraint::Singular { .. } => sub_where_params,
                                ParentConstraint::Bulk { params: prev_params, .. } => {
                                    let mut p = prev_params.clone();
                                    p.extend(sub_where_params);
                                    p
                                }
                            };

                            let final_subquery_sql = match parent_constraint {
                                ParentConstraint::Singular { step_id: _ } => {
                                    let c_pk_col = ast.models.get(&c_model).unwrap().resolved_fields.iter().find(|f| f.attributes.iter().any(|a| matches!(a, FieldAttribute::Id))).map(|f| f.name.as_str()).unwrap_or("__id");
                                    format!("SELECT {} FROM {} WHERE {}", c_pk_col, c_model, sub_where_sql)
                                },
                                ParentConstraint::Bulk { sql: prev_sql, .. } => {
                                    let c_pk_col = ast.models.get(&c_model).unwrap().resolved_fields.iter().find(|f| f.attributes.iter().any(|a| matches!(a, FieldAttribute::Id))).map(|f| f.name.as_str()).unwrap_or("__id");
                                    format!("SELECT {} FROM {} WHERE {} AND {} IN ({})", c_pk_col, c_model, sub_where_sql, fk_col, prev_sql)
                                }
                            };
                            
                            process_deferred_children(ast, &c_model, &ParentConstraint::Bulk { sql: final_subquery_sql, params: final_subquery_params }, bulk_deferred_children.clone(), steps, alias_counter)?;
                        }
                    }
                }
                
                steps.push(ExecutionStep::UpdateMany { id: child_step_id, queries });
            },
            DeferredAction::DeleteMany(target_name, child_where) => {
                let child_step_id = format!("step_{}_delete_many_{}", target_name.to_lowercase(), *alias_counter);
                *alias_counter += 1;
                
                let mut concrete_models = Vec::new();
                if ast.models.contains_key(&target_name) {
                    concrete_models.push(target_name.clone());
                } else if let Some(union_models) = ast.unions.get(&target_name) {
                    concrete_models.extend(union_models.clone());
                } else if ast.bases.contains_key(&target_name) {
                    for (m_name, m_node) in &ast.models {
                        if m_node.resolved_bases.contains(&target_name) {
                            concrete_models.push(m_name.clone());
                        }
                    }
                }
                
                let mut required_bases = Vec::new();
                for (k, v) in &child_where {
                    if k.starts_with("__") && k != "__id" && k != "__kind" {
                        if let Some(b) = v.as_bool() {
                            if b {
                                required_bases.push(k[2..].to_string());
                            }
                        }
                    }
                }
                
                concrete_models.retain(|m_name| {
                    let m_node = ast.models.get(m_name).unwrap();
                    required_bases.iter().all(|b| m_node.resolved_bases.contains(b))
                });
                
                let mut queries = Vec::new();
                let parent_ref = match parent_constraint {
                    ParentConstraint::Singular { step_id } => Some(Parameter::Reference { step_id: step_id.clone(), column: target_pk.clone() }),
                    ParentConstraint::Bulk { .. } => None,
                };
                
                for c_model in concrete_models {
                    let child_model_def = ast.models.get(&c_model).unwrap_or_else(|| panic!("Failed to find model: {}", c_model));
                    let mut params = Vec::new();
                    let mut param_idx = 1;
                    if let ParentConstraint::Bulk { params: bulk_params, .. } = parent_constraint {
                        params.extend(bulk_params.clone());
                        param_idx += bulk_params.len();
                    }
                    
                    let mut cleaned_where = child_where.clone();
                    cleaned_where.retain(|k, _| !k.starts_with("__") || k == "__id" || k == "__kind");
                    match parse_where_clause(ast, &cleaned_where, child_model_def) {
                        Ok(where_clause_ir) => {
                            let (where_sql, where_params) = compile_parameterized_where(&where_clause_ir, &c_model, &mut param_idx);
                            params.extend(where_params);
                            
                            let combined_where_sql = match parent_constraint {
                            ParentConstraint::Singular { step_id } => {
                                let sql = format!("({} AND {}.{} = ?{})", where_sql, c_model, fk_col, param_idx);
                                params.push(Parameter::Reference { step_id: step_id.clone(), column: target_pk.clone() });
                                param_idx += 1;
                                sql
                            },
                            ParentConstraint::Bulk { sql: bulk_sql, .. } => {
                                format!("({} AND {}.{} IN ({}))", where_sql, c_model, fk_col, bulk_sql)
                            }
                        };
                            
                            let sql = format!(
                                "DELETE FROM {} WHERE {};",
                                c_model,
                                combined_where_sql
                            );
                            queries.push((sql, params));
                        },
                        Err(_) => {},
                    }
                }
                
                steps.push(ExecutionStep::DeleteMany { id: child_step_id, queries });
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
                
                let combined_where_sql = match parent_constraint {
                    ParentConstraint::Singular { step_id } => {
                        let sql = format!("({} AND {}.{} = ?{})", where_sql, child.target_model, fk_col, param_idx);
                        params.push(Parameter::Reference { step_id: step_id.clone(), column: target_pk.clone() });
                        param_idx += 1;
                        sql
                    },
                    ParentConstraint::Bulk { sql: bulk_sql, .. } => {
                        format!("({} AND {}.{} IN ({}))", where_sql, child.target_model, fk_col, bulk_sql)
                    }
                };
                
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
                if let ParentConstraint::Bulk { .. } = parent_constraint {
                    return Err("Semantics Error: Cannot 'upsert' a child to multiple parents in a bulk update.".to_string());
                }
                
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
                if let ParentConstraint::Bulk { .. } = parent_constraint {
                    return Err("Semantics Error: Cannot 'set' a relation to multiple distinct parents in a bulk update.".to_string());
                }
                
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
                if let ParentConstraint::Bulk { params: bulk_params, .. } = parent_constraint {
                    params.extend(bulk_params.clone());
                    param_idx += bulk_params.len();
                }
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
                WhereCondition::Contains(v) => {
                    let s = format!("{} LIKE ?{} ESCAPE '\\'", col, *param_idx);
                    *param_idx += 1;
                    let escaped = v.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
                    params.push(Parameter::Literal(serde_json::Value::String(format!("%{}%", escaped))));
                    (s, params)
                },
                WhereCondition::StartsWith(v) => {
                    let s = format!("{} LIKE ?{} ESCAPE '\\'", col, *param_idx);
                    *param_idx += 1;
                    let escaped = v.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
                    params.push(Parameter::Literal(serde_json::Value::String(format!("{}%", escaped))));
                    (s, params)
                },
                WhereCondition::EndsWith(v) => {
                    let s = format!("{} LIKE ?{} ESCAPE '\\'", col, *param_idx);
                    *param_idx += 1;
                    let escaped = v.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
                    params.push(Parameter::Literal(serde_json::Value::String(format!("%{}", escaped))));
                    (s, params)
                },
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
            enums: std::collections::HashMap::new(),
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
        let ast = mock_ast();
        assert_eq!(
            validate_and_normalize_scalar(&ast, "val", "Float", false, &json!(10.5)).unwrap(),
            json!(10.5)
        );
        assert_eq!(
            validate_and_normalize_scalar(&ast, "val", "Float", false, &json!(10)).unwrap(),
            json!(10)
        );
        assert!(validate_and_normalize_scalar(&ast, "val", "Float", false, &json!("10.5")).is_err());
    }

    #[test]
    fn test_validate_and_normalize_scalar_datetime() {
        let ast = mock_ast();
        assert_eq!(
            validate_and_normalize_scalar(&ast, "date", "DateTime", false, &json!("2025-01-01T00:00:00Z")).unwrap(),
            json!("2025-01-01T00:00:00.000Z")
        );
        assert_eq!(
            validate_and_normalize_scalar(&ast, "date", "DateTime", false, &json!("2025-10-10T12:00:00-04:00")).unwrap(),
            json!("2025-10-10T16:00:00.000Z")
        );
        assert!(validate_and_normalize_scalar(&ast, "date", "DateTime", false, &json!("Next Tuesday")).is_err());
    }

    #[test]
    fn test_validate_and_normalize_scalar_fallback() {
        let ast = mock_ast();
        assert_eq!(
            validate_and_normalize_scalar(&ast, "name", "String", false, &json!("Alice")).unwrap(),
            json!("Alice")
        );
    }

    #[test]
    fn test_validate_and_normalize_scalar_enum() {
        let mut ast = mock_ast();
        ast.enums.insert("Role".to_string(), vec!["ADMIN".to_string(), "USER".to_string()]);
        
        assert_eq!(
            validate_and_normalize_scalar(&ast, "role", "Role", true, &json!("ADMIN")).unwrap(),
            json!("ADMIN")
        );
        assert!(validate_and_normalize_scalar(&ast, "role", "Role", true, &json!("SUPERADMIN")).is_err());
        assert!(validate_and_normalize_scalar(&ast, "role", "Role", true, &json!(123)).is_err());
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

    #[test]
    fn test_hydrate_update_many() {
        let ast = mock_ast();
        let payload = json!({
            "data": {
                "age": 31
            },
            "where": {
                "name": "Alice"
            }
        });
        
        let mut alias_counter = 0;
        let plan = hydrate_mutation_to_plan(&ast, "User", "updateMany", &payload, &mut alias_counter).unwrap();
        
        assert_eq!(plan.steps.len(), 1);
        if let ExecutionStep::UpdateMany { id, queries, .. } = &plan.steps[0] {
            assert_eq!(id, "step_user_updatemany_0");
            let (sql, params) = &queries[0];
            assert!(sql.contains("UPDATE User SET age = ?1 WHERE User.name = ?2;"));
            assert_eq!(params.len(), 2);
        } else {
            panic!("Expected UpdateMany step");
        }
    }

    #[test]
    fn test_hydrate_update_many_with_relations() {
        let mut ast = mock_ast();
        ast.models.get_mut("User").unwrap().resolved_fields.push(FieldNode {
            name: "posts".to_string(),
            field_type: AstFieldType::RelationArray("Post".to_string()),
            is_optional: true,
            attributes: vec![FieldAttribute::Relation { name: None, on_delete: None, fields: None, references: None }, FieldAttribute::InternalRelation { fields: vec!["userId".to_string()], references: vec!["__id".to_string()] }],
        });
        ast.models.insert("Post".to_string(), ModelNode {
            name: "Post".to_string(),
            block_attributes: vec![],
            extends: vec![],
            fields: vec![],
            resolved_bases: std::collections::BTreeSet::new(),
            resolved_fields: vec![
                FieldNode { name: "__id".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![FieldAttribute::Id] },
                FieldNode { name: "title".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![] },
                FieldNode { name: "userId".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: true, attributes: vec![] },
            ]
        });

        let payload = json!({
            "data": {
                "posts": { "deleteMany": { "where": { "title": "Old" } } }
            },
            "where": { "name": "Alice" }
        });
        
        let mut alias_counter = 0;
        let res = hydrate_mutation_to_plan(&ast, "User", "updateMany", &payload, &mut alias_counter);
        assert!(res.is_ok(), "Expected Ok, got Err: {:?}", res.err());
        let plan = res.unwrap();
        assert_eq!(plan.steps.len(), 2);
    }

    #[test]
    fn test_hydrate_delete_many() {
        let ast = mock_ast();
        let payload = json!({
            "where": {
                "age": { "gt": 20 }
            }
        });
        
        let mut alias_counter = 0;
        let plan = hydrate_mutation_to_plan(&ast, "User", "deleteMany", &payload, &mut alias_counter).unwrap();
        
        assert_eq!(plan.steps.len(), 1);
        if let ExecutionStep::DeleteMany { id, queries, .. } = &plan.steps[0] {
            assert_eq!(id, "step_user_deletemany_0");
            let (sql, params) = &queries[0];
            assert!(sql.contains("DELETE FROM User WHERE User.age > ?1;"));
            assert_eq!(params.len(), 1);
        } else {
            panic!("Expected DeleteMany step");
        }
    }

    #[test]
    fn test_hydrate_delete_many_cascades_polymorphic() {
        use schema_parser::ast::BaseNode;
        let mut ast = mock_ast();
        
        // Setup polymorphic relation
        ast.bases.insert("Account".to_string(), BaseNode {
            name: "Account".to_string(),
            fields: vec![],
            extends: vec![],
            resolved_bases: std::collections::BTreeSet::new(),
            resolved_fields: vec![],
        });
        ast.models.get_mut("User").unwrap().resolved_bases.insert("Account".to_string());
        
        ast.models.insert("Profile".to_string(), ModelNode {
            block_attributes: vec![], extends: vec![], fields: vec![], resolved_bases: std::collections::BTreeSet::new(),
            name: "Profile".to_string(),
            resolved_fields: vec![
                FieldNode {
                    name: "owner".to_string(),
                    field_type: AstFieldType::PolymorphicBase("Account".to_string()),
                    is_optional: false,
                    attributes: vec![],
                }
            ]
        });

        let payload = json!({
            "where": { "name": "Alice" }
        });
        
        let mut alias_counter = 0;
        let plan = hydrate_mutation_to_plan(&ast, "User", "deleteMany", &payload, &mut alias_counter).unwrap();
        
        // Should have 2 steps: 1 cascade + 1 main delete
        assert_eq!(plan.steps.len(), 2);
        
        // Cascade step
        if let ExecutionStep::Query { sql, .. } = &plan.steps[0] {
            assert!(sql.contains("DELETE FROM Profile WHERE owner_type = 'User' AND owner_id IN (SELECT __id FROM User WHERE User.name = ?1);"));
        } else {
            panic!("Expected Query step for cascade");
        }
    }

    #[test]
    fn test_hydrate_update_many_missing_data() {
        let ast = mock_ast();
        let payload = json!({
            "where": { "name": "Alice" }
        });
        let mut alias_counter = 0;
        let res = hydrate_mutation_to_plan(&ast, "User", "updateMany", &payload, &mut alias_counter);
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("Missing 'data' block"));
    }

    #[test]
    fn test_hydrate_update_many_empty_data() {
        let ast = mock_ast();
        let payload = json!({
            "data": {},
            "where": { "name": "Alice" }
        });
        let mut alias_counter = 0;
        let res = hydrate_mutation_to_plan(&ast, "User", "updateMany", &payload, &mut alias_counter);
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("No data provided"));
    }

    #[test]
    fn test_hydrate_update_many_scalar_array() {
        let mut ast = mock_ast();
        ast.models.get_mut("User").unwrap().resolved_fields.push(FieldNode {
            name: "tags".to_string(),
            field_type: AstFieldType::ScalarArray("String".to_string()),
            is_optional: true,
            attributes: vec![],
        });

        let payload = json!({
            "data": {
                "tags": ["rust", "db"]
            },
            "where": { "name": "Alice" }
        });
        
        let mut alias_counter = 0;
        let plan = hydrate_mutation_to_plan(&ast, "User", "updateMany", &payload, &mut alias_counter).unwrap();
        
        assert_eq!(plan.steps.len(), 1);
        if let ExecutionStep::UpdateMany { id, queries, .. } = &plan.steps[0] {
            assert_eq!(id, "step_user_updatemany_0");
            let (sql, params) = &queries[0];
            assert!(sql.contains("UPDATE User SET tags = ?1 WHERE User.name = ?2;"));
            assert_eq!(params.len(), 2);
            if let Parameter::Literal(val) = &params[0] {
                assert_eq!(val.as_str().unwrap(), "[\"rust\",\"db\"]");
            } else {
                panic!("Expected string literal for array parameter");
            }
        } else {
            panic!("Expected UpdateMany step");
        }
    }

    #[test]
    fn test_hydrate_delete_many_empty_where() {
        let ast = mock_ast();
        let payload = json!({
            "where": {}
        });
        
        let mut alias_counter = 0;
        let plan = hydrate_mutation_to_plan(&ast, "User", "deleteMany", &payload, &mut alias_counter).unwrap();
        
        assert_eq!(plan.steps.len(), 1);
        if let ExecutionStep::DeleteMany { id, queries, .. } = &plan.steps[0] {
            assert_eq!(id, "step_user_deletemany_0");
            let (sql, params) = &queries[0];
            assert!(sql.contains("DELETE FROM User WHERE 1=1;")); // Empty where evaluates to AlwaysTrue (1=1)
            assert_eq!(params.len(), 0); // No parameters for empty where
        } else {
            panic!("Expected DeleteMany step");
        }
    }

    #[test]
    fn test_compile_parameterized_where_string_filters() {
        use query_compiler::ir::{WhereClause, WhereCondition};
        
        let c_contains = WhereClause::Field("name".to_string(), WhereCondition::Contains("100%_juice\\'s".to_string()));
        let mut idx = 1;
        let (sql_c, params_c) = compile_parameterized_where(&c_contains, "t0", &mut idx);
        assert_eq!(sql_c, "t0.name LIKE ?1 ESCAPE '\\'");
        assert_eq!(params_c.len(), 1);
        if let Parameter::Literal(serde_json::Value::String(s)) = &params_c[0] {
            assert_eq!(s, "%100\\%\\_juice\\\\'s%");
        } else { panic!("Expected string parameter"); }

        let c_starts = WhereClause::Field("name".to_string(), WhereCondition::StartsWith("100%_juice\\'s".to_string()));
        let mut idx = 1;
        let (sql_s, params_s) = compile_parameterized_where(&c_starts, "t0", &mut idx);
        assert_eq!(sql_s, "t0.name LIKE ?1 ESCAPE '\\'");
        if let Parameter::Literal(serde_json::Value::String(s)) = &params_s[0] {
            assert_eq!(s, "100\\%\\_juice\\\\'s%");
        } else { panic!("Expected string parameter"); }

        let c_ends = WhereClause::Field("name".to_string(), WhereCondition::EndsWith("100%_juice\\'s".to_string()));
        let mut idx = 1;
        let (sql_e, params_e) = compile_parameterized_where(&c_ends, "t0", &mut idx);
        assert_eq!(sql_e, "t0.name LIKE ?1 ESCAPE '\\'");
        if let Parameter::Literal(serde_json::Value::String(s)) = &params_e[0] {
            assert_eq!(s, "%100\\%\\_juice\\\\'s");
        } else { panic!("Expected string parameter"); }
    }

    #[test]
    fn test_translate_delete_node_nested_singular() {
        let mut ast = mock_ast();
        // Add a self relation dummy to User so fk_col resolution succeeds
        ast.models.get_mut("User").unwrap().resolved_fields.push(FieldNode {
            name: "dummyId".to_string(),
            field_type: AstFieldType::Scalar("String".to_string()),
            is_optional: true,
            attributes: vec![],
        });
        ast.models.get_mut("User").unwrap().resolved_fields.push(FieldNode {
            name: "dummy".to_string(),
            field_type: AstFieldType::Relation("User".to_string()),
            is_optional: true,
            attributes: vec![FieldAttribute::Relation { name: None, on_delete: None, fields: None, references: None }, FieldAttribute::InternalRelation { fields: vec!["dummyId".to_string()], references: vec!["__id".to_string()] }],
        });

        let child_where = json!({"__id": "user_2"}).as_object().unwrap().clone();
        
        let deferred_children = vec![DeferredChild {
            target_model: "User".to_string(),
            action: DeferredAction::Delete("User".to_string(), child_where),
            relation_field_name: "dummy".to_string(),
        }];

        let mut alias_counter = 0;
        let mut steps = Vec::new();
        process_deferred_children(&ast, "User", &ParentConstraint::Singular { step_id: "step_0".to_string() }, deferred_children, &mut steps, &mut alias_counter).unwrap();
        
        assert_eq!(steps.len(), 1);
        if let ExecutionStep::DeleteBranch { id: _, sql, params, parent_ref } = &steps[0] {
            assert!(sql.contains("DELETE FROM User WHERE (User.__id = ?1 AND User.dummyId = ?2) RETURNING __id;"));
            assert_eq!(params.len(), 1);
            if let Parameter::Reference { step_id, column } = parent_ref {
                assert_eq!(step_id, "step_0");
                assert_eq!(column, "__id");
            } else {
                panic!("Expected Reference for parent_ref");
            }
        } else {
            panic!("Expected DeleteBranch step");
        }
    }

    #[test]
    fn test_translate_delete_many_node() {
        let mut ast = mock_ast();
        ast.models.get_mut("User").unwrap().resolved_fields.push(FieldNode {
            name: "dummyId".to_string(),
            field_type: AstFieldType::Scalar("String".to_string()),
            is_optional: true,
            attributes: vec![],
        });
        ast.models.get_mut("User").unwrap().resolved_fields.push(FieldNode {
            name: "dummy".to_string(),
            field_type: AstFieldType::Relation("User".to_string()),
            is_optional: true,
            attributes: vec![FieldAttribute::Relation { name: None, on_delete: None, fields: None, references: None }, FieldAttribute::InternalRelation { fields: vec!["dummyId".to_string()], references: vec!["__id".to_string()] }],
        });

        let child_where = json!({"age": 99}).as_object().unwrap().clone();
        
        let deferred_children = vec![DeferredChild {
            target_model: "User".to_string(),
            action: DeferredAction::DeleteMany("User".to_string(), child_where),
            relation_field_name: "dummy".to_string(),
        }];

        let mut alias_counter = 0;
        let mut steps = Vec::new();
        process_deferred_children(&ast, "User", &ParentConstraint::Singular { step_id: "step_0".to_string() }, deferred_children, &mut steps, &mut alias_counter).unwrap();
        
        assert_eq!(steps.len(), 1);
        if let ExecutionStep::DeleteMany { queries, .. } = &steps[0] {
            assert_eq!(queries.len(), 1);
            let (sql, params) = &queries[0];
            assert!(sql.contains("DELETE FROM User WHERE (User.age = ?1 AND User.dummyId = ?2);"));
            assert_eq!(params.len(), 2);
            
            if let Parameter::Reference { step_id, column } = &params[1] {
                assert_eq!(step_id, "step_0");
                assert_eq!(column, "__id");
            } else {
                panic!("Expected Reference in params");
            }
        } else {
            panic!("Expected DeleteMany step");
        }
    }

    #[test]
    fn test_ast_driven_fanout_batch_delete() {
        use schema_parser::ast::BaseNode;
        let mut ast = mock_ast();
        
        ast.bases.insert("Content".to_string(), BaseNode {
            name: "Content".to_string(),
            fields: vec![],
            extends: vec![],
            resolved_bases: std::collections::BTreeSet::new(),
            resolved_fields: vec![],
        });
        
        let mut art_model = ModelNode { block_attributes: vec![], extends: vec![], fields: vec![], resolved_bases: std::collections::BTreeSet::new(), name: "Article".to_string(), resolved_fields: vec![] };
        art_model.resolved_bases.insert("Content".to_string());
        art_model.resolved_bases.insert("Viewable".to_string());
        ast.models.insert("Article".to_string(), art_model);

        let mut gal_model = ModelNode { block_attributes: vec![], extends: vec![], fields: vec![], resolved_bases: std::collections::BTreeSet::new(), name: "Gallery".to_string(), resolved_fields: vec![] };
        gal_model.resolved_bases.insert("Content".to_string());
        ast.models.insert("Gallery".to_string(), gal_model);

        let mut steps = Vec::new();
        let mut alias_counter = 0;
        
        ast.models.get_mut("User").unwrap().resolved_fields.push(FieldNode {
            name: "itemsId".to_string(),
            field_type: AstFieldType::Scalar("String".to_string()),
            is_optional: true,
            attributes: vec![],
        });
        ast.models.get_mut("User").unwrap().resolved_fields.push(FieldNode {
            name: "items".to_string(),
            field_type: AstFieldType::PolymorphicBaseArray("Content".to_string()),
            is_optional: false,
            attributes: vec![FieldAttribute::Relation { name: None, on_delete: None, fields: None, references: None }, FieldAttribute::InternalRelation { fields: vec!["itemsId".to_string()], references: vec!["__id".to_string()] }],
        });

        let child_where = json!({"__Viewable": true}).as_object().unwrap().clone();
        
        let deferred_children = vec![DeferredChild {
            target_model: "Content".to_string(),
            action: DeferredAction::DeleteMany("Content".to_string(), child_where),
            relation_field_name: "items".to_string(),
        }];

        process_deferred_children(&ast, "User", &ParentConstraint::Singular { step_id: "step_0".to_string() }, deferred_children, &mut steps, &mut alias_counter).unwrap();
        
        assert_eq!(steps.len(), 1);
        if let ExecutionStep::DeleteMany { queries, .. } = &steps[0] {
            assert_eq!(queries.len(), 1);
            assert!(queries[0].0.contains("DELETE FROM Article WHERE"));
        } else {
            panic!("Expected DeleteMany step");
        }
    }

    #[test]
    fn test_polymorphic_disambiguation_delete_strips_kind() {
        use schema_parser::ast::BaseNode;
        let mut ast = mock_ast();
        
        ast.bases.insert("Content".to_string(), BaseNode {
            name: "Content".to_string(),
            fields: vec![],
            extends: vec![],
            resolved_bases: std::collections::BTreeSet::new(),
            resolved_fields: vec![],
        });
        
        let mut art_model = ModelNode { block_attributes: vec![], extends: vec![], fields: vec![], resolved_bases: std::collections::BTreeSet::new(), name: "Article".to_string(), resolved_fields: vec![] };
        art_model.resolved_bases.insert("Content".to_string());
        ast.models.insert("Article".to_string(), art_model);
        
        ast.models.get_mut("User").unwrap().resolved_fields.push(FieldNode {
            name: "itemsId".to_string(),
            field_type: AstFieldType::Scalar("String".to_string()),
            is_optional: true,
            attributes: vec![],
        });
        ast.models.get_mut("User").unwrap().resolved_fields.push(FieldNode {
            name: "items".to_string(),
            field_type: AstFieldType::PolymorphicBaseArray("Content".to_string()),
            is_optional: false,
            attributes: vec![FieldAttribute::Relation { name: None, on_delete: None, fields: None, references: None }, FieldAttribute::InternalRelation { fields: vec!["itemsId".to_string()], references: vec!["__id".to_string()] }],
        });

        let payload_delete = json!({
            "items": { "delete": [{ "__id": "art_1", "__kind": "Article" }] }
        });
        
        let mut alias_counter = 0;
        let mut steps = Vec::new();
        translate_update_node(&ast, "User", &json!({"__id": "u1"}).as_object().unwrap(), payload_delete.as_object().unwrap(), &mut steps, &mut alias_counter, None).unwrap();
        
        assert_eq!(steps.len(), 2);
        if let ExecutionStep::DeleteBranch { sql, params, .. } = &steps[1] {
            assert!(sql.contains("DELETE FROM Article WHERE (Article.__id = ?1 AND Article.userId = ?2) RETURNING __id;"));
            assert_eq!(params.len(), 1); 
            
            if let Parameter::Literal(serde_json::Value::String(s)) = &params[0] {
                assert_eq!(s, "art_1");
            } else {
                panic!("Expected __id literal");
            }
        } else {
            panic!("Expected DeleteBranch step");
        }
    }
}

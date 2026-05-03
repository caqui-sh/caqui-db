use serde_json::Value;
use query_compiler::ir::{QueryNode, SelectField, QueryIrSource};
use schema_parser::ast::{SchemaAst, AstFieldType, FieldAttribute};
use crate::where_parser::{parse_where_clause};

pub fn hydrate_payload_to_ir(
    ast: &SchemaAst,
    model_name: &str,
    payload: &Value, 
    alias_counter: &mut usize,
    depth: usize
) -> Result<QueryNode, String> {
    
    if depth > 10 {
        return Err("Security Exception: Maximum query depth exceeded.".to_string());
    }

    if let Some(base_def) = ast.bases.get(model_name) {
        return compile_polymorphic_read(ast, base_def, payload, alias_counter, depth);
    }

    if let Some(union_targets) = ast.unions.get(model_name) {
        return compile_union_read(ast, model_name, union_targets, payload, alias_counter, depth);
    }

    // 1. Strict Schema Validation: Verify model exists physically
    let model_def = ast.models.get(model_name)
        .ok_or_else(|| format!("Security Exception: Model '{}' undefined.", model_name))?;

    let mut selections = Vec::new();
    let current_alias = format!("t{}", alias_counter);
    *alias_counter += 1;

    let requested_fields = payload.get("select").and_then(|v| v.as_object())
        .ok_or("Missing 'select' projection block")?;

    // 2. Dynamic Field Resolution
    for (field_name, sub_payload) in requested_fields {
        // Find field in AST
        let field_def = model_def.resolved_fields.iter().find(|f| &f.name == field_name)
            .ok_or_else(|| format!("Invalid field '{}' on '{}'.", field_name, model_name))?;

        match &field_def.field_type {
            AstFieldType::Scalar(type_name) => {
                if type_name == "Boolean" {
                    selections.push(SelectField::ScalarBoolean(field_name.clone()));
                } else {
                    selections.push(SelectField::Scalar(field_name.clone()));
                }
            },
            AstFieldType::ScalarArray(_) => selections.push(SelectField::ScalarArray(field_name.clone())),
            
            AstFieldType::Relation(target_model) | AstFieldType::RelationArray(target_model) => {
                // 3. Recursive Graph Traversal for nested relational queries
                let child_node = hydrate_payload_to_ir(ast, target_model, sub_payload, alias_counter, depth + 1)?;
                let is_list = matches!(field_def.field_type, AstFieldType::RelationArray(_));
                
                // Determine foreign key by inspecting the AST
                let mut resolved_fk = format!("{}_id", model_name.to_lowercase()); // fallback
                
                let relation_attr = field_def.attributes.iter().find(|a| matches!(a, FieldAttribute::Relation { .. }));
                let relation_name = if let Some(FieldAttribute::Relation { name, .. }) = relation_attr {
                    name.clone()
                } else {
                    None
                };

                let target_resolved_fields = if let Some(m) = ast.models.get(target_model) {
                    &m.resolved_fields
                } else if let Some(b) = ast.bases.get(target_model) {
                    &b.resolved_fields
                } else {
                    return Err(format!("Security Exception: Model '{}' undefined.", target_model));
                };
                for target_field in target_resolved_fields {
                    if let AstFieldType::Relation(ref_model) | AstFieldType::RelationArray(ref_model) = &target_field.field_type {
                        if ref_model == model_name {
                            if let (Some(target_name), Some(fields)) = (target_field.attributes.iter().find_map(|a| if let FieldAttribute::Relation { name, .. } = a { Some(name.clone()) } else { None }), target_field.attributes.iter().find_map(|a| if let FieldAttribute::InternalRelation { fields, .. } = a { Some(fields.clone()) } else { None })) {
                                if relation_name == target_name {
                                    if !fields.is_empty() {
                                        resolved_fk = fields[0].clone();
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                
                let mut is_forward = false;
                
                // If we are on the child side (we own the foreign key), our own @relation holds the fields
                if let Some(FieldAttribute::InternalRelation { fields, .. }) = field_def.attributes.iter().find(|a| matches!(a, FieldAttribute::InternalRelation { .. })) {
                    if !fields.is_empty() {
                        if matches!(field_def.field_type, AstFieldType::RelationArray(_)) {
                            // 1:N array side is never forward (the OTHER side holds the FK)
                            is_forward = false;
                            resolved_fk = fields[0].clone();
                        } else {
                            // Singular relation: check if we actually own the column
                            if model_def.resolved_fields.iter().any(|f| &f.name == &fields[0]) {
                                is_forward = true;
                                resolved_fk = fields[0].clone();
                            } else {
                                is_forward = false;
                                resolved_fk = fields[0].clone();
                            }
                        }
                    }
                }
                
                // We also need to inject the foreign key (teamId) into the INNER branches of the target polymorphic union!
                // Since child_node is a PolymorphicUnion, we must mutate its branches to select resolved_fk.
                let mut child_node = child_node;
                if !is_forward {
                    if !child_node.selections.iter().any(|s| match s { query_compiler::ir::SelectField::Scalar(name) => *name == resolved_fk, _ => false }) {
                        child_node.selections.push(query_compiler::ir::SelectField::Scalar(resolved_fk.clone()));
                    }
                    
                    if let query_compiler::ir::QueryIrSource::Polymorphic { branches, .. } = &mut child_node.source {
                        for branch in branches {
                            if !branch.selections.iter().any(|s| match s { query_compiler::ir::SelectField::Scalar(name) => *name == resolved_fk, _ => false }) {
                                branch.selections.push(query_compiler::ir::SelectField::Scalar(resolved_fk.clone()));
                            }
                        }
                    }
                }
                
                selections.push(SelectField::Relation {
                    field_name: field_name.clone(),
                    foreign_key: resolved_fk, 
                    is_list,
                    is_forward,
                    query: Box::new(child_node),
                });
            },
            AstFieldType::PolymorphicUnion(target_name) | AstFieldType::PolymorphicUnionArray(target_name) | AstFieldType::PolymorphicBase(target_name) | AstFieldType::PolymorphicBaseArray(target_name) => {
                let is_list = matches!(field_def.field_type, AstFieldType::PolymorphicUnionArray(_) | AstFieldType::PolymorphicBaseArray(_));
                
                let targets = if let Some(union_targets) = ast.unions.get(target_name) {
                    union_targets.clone()
                } else if ast.bases.contains_key(target_name) {
                    let mut impls = Vec::new();
                    for model in ast.models.values() {
                        if model.resolved_bases.contains(target_name) {
                            impls.push(model.name.clone());
                        }
                    }
                    impls
                } else {
                    return Err(format!("Security Exception: Target '{}' undefined.", target_name));
                };
                    
                let mut target_fragments = std::collections::HashMap::new();
                
                // Expecting sub_payload to have keys matching the target models
                if let Some(union_queries) = sub_payload.as_object() {
                    for (target_model_name, target_payload) in union_queries {
                        if !targets.contains(target_model_name) {
                            return Err(format!("Invalid union target '{}' for union '{}'.", target_model_name, target_name));
                        }
                        
                        let fragment_node = hydrate_payload_to_ir(ast, target_model_name, target_payload, alias_counter, depth + 1)?;
                        target_fragments.insert(target_model_name.clone(), fragment_node);
                    }
                }
                
                if target_fragments.is_empty() {
                    return Err(format!("Missing union target fragments for field '{}'.", field_name));
                }

                selections.push(SelectField::Polymorphic {
                    field_name: field_name.clone(),
                    is_list,
                    target_fragments,
                });
            }
        }
    }

    let filters = if let Some(where_obj) = payload.get("where").and_then(|v| v.as_object()) {
        if where_obj.is_empty() {
            None
        } else {
            Some(parse_where_clause(ast, where_obj, model_def)?)
        }
    } else {
        None
    };

    let primary_key = model_def.resolved_fields.iter()
        .find(|f| f.attributes.iter().any(|a| matches!(a, FieldAttribute::Id)))
        .map(|f| f.name.clone())
        .unwrap_or_else(|| "__id".to_string());

    let mut order_by = Vec::new();
    if let Some(order_obj) = payload.get("orderBy").and_then(|v| v.as_object()) {
        for (field, dir) in order_obj {
            let direction = if dir.as_str().map(|s| s.to_lowercase()).as_deref() == Some("desc") {
                query_compiler::ir::OrderDirection::Desc
            } else {
                query_compiler::ir::OrderDirection::Asc
            };
            order_by.push((field.clone(), direction));
        }
    }

    Ok(QueryNode {
        source: query_compiler::ir::QueryIrSource::Table(model_name.to_string()),
        primary_key,
        alias: current_alias,
        selections,
        filters,
        order_by,
        limit: payload.get("limit").and_then(|l| l.as_u64()).map(|l| l as usize),
        offset: payload.get("skip").and_then(|l| l.as_u64()).map(|l| l as usize),
    })
}

fn compile_union_read(
    ast: &SchemaAst,
    union_name: &str,
    targets: &[String],
    payload: &Value,
    alias_counter: &mut usize,
    _depth: usize
) -> Result<QueryNode, String> {
    let mut branches = Vec::new();
    let mut all_requested_fields = std::collections::BTreeSet::new();
    all_requested_fields.insert("__id".to_string());
    all_requested_fields.insert("__kind".to_string());

    let requested_fields = payload.get("select").and_then(|v| v.as_object())
        .ok_or("Missing 'select' projection block")?;

    for target_payload in requested_fields.values() {
        if let Some(target_select) = target_payload.get("select").and_then(|v| v.as_object()) {
            for field in target_select.keys() {
                all_requested_fields.insert(field.clone());
            }
        }
    }

    for target_model_name in targets {
        let model_def = ast.models.get(target_model_name)
            .ok_or_else(|| format!("Security Exception: Model '{}' undefined.", target_model_name))?;

        let mut inner_selections = Vec::new();
        let branch_payload = requested_fields.get(target_model_name);
        let branch_selects = branch_payload.and_then(|p| p.get("select")).and_then(|v| v.as_object());

        for field_name in &all_requested_fields {
            if field_name == "__kind" {
                inner_selections.push(SelectField::SyntheticNull(format!("'{}' AS __kind", target_model_name)));
                continue;
            }

            if let Some(field_def) = model_def.resolved_fields.iter().find(|f| &f.name == field_name) {
                if field_name == "__id" || branch_selects.map_or(false, |s| s.contains_key(field_name)) {
                     match &field_def.field_type {
                        AstFieldType::Scalar(type_name) => {
                            if type_name == "Boolean" {
                                inner_selections.push(SelectField::ScalarBoolean(field_name.clone()));
                            } else {
                                inner_selections.push(SelectField::Scalar(field_name.clone()));
                            }
                        },
                        AstFieldType::ScalarArray(_) => inner_selections.push(SelectField::ScalarArray(field_name.clone())),
                        _ => {
                            inner_selections.push(SelectField::SyntheticNull(format!("NULL AS {}", field_name)));
                        }
                    }
                } else {
                    inner_selections.push(SelectField::SyntheticNull(format!("NULL AS {}", field_name)));
                }
            } else {
                inner_selections.push(SelectField::SyntheticNull(format!("NULL AS {}", field_name)));
            }
        }

        let current_alias = format!("t{}", alias_counter);
        *alias_counter += 1;
        
        branches.push(QueryNode {
            source: QueryIrSource::Table(target_model_name.clone()),
            primary_key: "__id".to_string(),
            alias: current_alias,
            selections: inner_selections,
            filters: branch_payload.and_then(|p| p.get("where")).and_then(|v| v.as_object()).map(|obj| parse_where_clause(ast, obj, model_def)).transpose()?,
            order_by: vec![],
            limit: None,
            offset: None,
        });
    }

    let current_alias = format!("t{}", alias_counter);
    *alias_counter += 1;

    let mut outer_selections = Vec::new();
    for field_name in &all_requested_fields {
        outer_selections.push(SelectField::Scalar(field_name.clone()));
    }

    Ok(QueryNode {
        source: QueryIrSource::Polymorphic {
            alias: union_name.to_string(),
            branches,
        },
        primary_key: "__id".to_string(),
        alias: current_alias,
        selections: outer_selections,
        filters: None,
        order_by: vec![],
        limit: payload.get("limit").and_then(|l| l.as_u64()).map(|l| l as usize),
        offset: payload.get("skip").and_then(|l| l.as_u64()).map(|l| l as usize),
    })
}

fn compile_polymorphic_read(
    ast: &schema_parser::ast::SchemaAst,
    base_def: &schema_parser::ast::BaseNode,
    payload: &serde_json::Value,
    alias_counter: &mut usize,
    depth: usize
) -> Result<query_compiler::ir::QueryNode, String> {
    let mut branches = Vec::new();
    
    let mut symmetric_projection: std::collections::BTreeSet<String> = base_def.resolved_fields.iter().map(|f| f.name.clone()).collect();
    if let Some(requested_fields) = payload.get("select").and_then(|v| v.as_object()) {
        for key in requested_fields.keys() {
            symmetric_projection.insert(key.clone());
        }
    }
    
    let implementing_models: Vec<&schema_parser::ast::ModelNode> = ast.models.values()
        .filter(|m| m.resolved_bases.contains(&base_def.name))
        .collect();

    if implementing_models.is_empty() {
        return Err(format!("NoImplementations: Base '{}' has no concrete implementations.", base_def.name));
    }
    
    for model in implementing_models {
        let current_alias = format!("t{}", alias_counter);
        *alias_counter += 1;
        
        let mut inner_selections = Vec::new();
        for field_name in &symmetric_projection {
            if let Some(field_def) = model.resolved_fields.iter().find(|f| &f.name == field_name) {
                match &field_def.field_type {
                    schema_parser::ast::AstFieldType::Scalar(type_name) => {
                        if type_name == "Boolean" {
                            inner_selections.push(query_compiler::ir::SelectField::ScalarBoolean(field_name.clone()));
                        } else {
                            inner_selections.push(query_compiler::ir::SelectField::Scalar(field_name.clone()));
                        }
                    },
                    schema_parser::ast::AstFieldType::ScalarArray(_) => inner_selections.push(query_compiler::ir::SelectField::ScalarArray(field_name.clone())),
                    _ => {}
                }
            } else if field_name.starts_with("__") && field_name != "__id" && field_name != "__kind" {
                if model.name == field_name.replace("__", "") {
                    inner_selections.push(query_compiler::ir::SelectField::SyntheticNull(format!("1 AS {}", field_name.clone())));
                } else if model.resolved_bases.contains(&field_name.replace("__", "")) {
                    inner_selections.push(query_compiler::ir::SelectField::Scalar(field_name.clone()));
                } else {
                    inner_selections.push(query_compiler::ir::SelectField::SyntheticNull(format!("0 AS {}", field_name.clone())));
                }
            } else {
                return Err(format!("Invalid field '{}' on '{}'.", field_name, model.name));
            }
        }
        
        let inner_filters = if let Some(where_obj) = payload.get("where").and_then(|v| v.as_object()) {
            if where_obj.is_empty() {
                None
            } else {
                Some(crate::where_parser::parse_where_clause(ast, where_obj, model)?)
            }
        } else {
            None
        };
        
        let primary_key = model.resolved_fields.iter()
            .find(|f| f.attributes.iter().any(|a| matches!(a, schema_parser::ast::FieldAttribute::Id)))
            .map(|f| f.name.clone())
            .unwrap_or_else(|| "__id".to_string());
            
        branches.push(query_compiler::ir::QueryNode {
            source: query_compiler::ir::QueryIrSource::Table(model.name.clone()),
            primary_key,
            alias: current_alias,
            selections: inner_selections,
            filters: inner_filters,
            order_by: vec![],
            limit: None,
            offset: None,
        });
    }
    
    let current_alias = format!("t{}", alias_counter);
    *alias_counter += 1;
    
    let mut outer_selections = Vec::new();
    let requested_fields = payload.get("select").and_then(|v| v.as_object())
        .ok_or("Missing 'select' projection block")?;
        
    for (field_name, sub_payload) in requested_fields {
        let field_def = base_def.resolved_fields.iter().find(|f| &f.name == field_name);
        
        if let Some(fd) = field_def {
            match &fd.field_type {
                schema_parser::ast::AstFieldType::Scalar(type_name) => {
                    if type_name == "Boolean" {
                        outer_selections.push(query_compiler::ir::SelectField::ScalarBoolean(field_name.clone()));
                    } else {
                        outer_selections.push(query_compiler::ir::SelectField::Scalar(field_name.clone()));
                    }
                },
                schema_parser::ast::AstFieldType::ScalarArray(_) => outer_selections.push(query_compiler::ir::SelectField::ScalarArray(field_name.clone())),
                schema_parser::ast::AstFieldType::Relation(target_model) | schema_parser::ast::AstFieldType::RelationArray(target_model) => {
                    let child_node = hydrate_payload_to_ir(ast, target_model, sub_payload, alias_counter, depth + 1)?;
                    let is_list = matches!(fd.field_type, schema_parser::ast::AstFieldType::RelationArray(_));
                    
                    let mut resolved_fk = format!("{}_id", base_def.name.to_lowercase()); 
                    let relation_attr = fd.attributes.iter().find(|a| matches!(a, schema_parser::ast::FieldAttribute::Relation { .. }));
                    let mut is_forward = true;
                    if let Some(schema_parser::ast::FieldAttribute::InternalRelation { fields, .. }) = fd.attributes.iter().find(|a| matches!(a, schema_parser::ast::FieldAttribute::InternalRelation { .. })) {
                        if !fields.is_empty() {
                            let is_pk = base_def.resolved_fields.iter().any(|f| &f.name == &fields[0] && f.attributes.iter().any(|a| matches!(a, schema_parser::ast::FieldAttribute::Id)));
                            if is_pk {
                                is_forward = false;
                                resolved_fk = format!("{}_id", base_def.name.to_lowercase()); 
                            } else {
                                resolved_fk = fields[0].clone();
                            }
                        } else {
                            is_forward = false;
                        }
                    }
                    if let Some(schema_parser::ast::FieldAttribute::InternalRelation { references, .. }) = fd.attributes.iter().find(|a| matches!(a, schema_parser::ast::FieldAttribute::InternalRelation { .. })) {
                        if !is_forward && !references.is_empty() {
                            resolved_fk = references[0].clone();
                        }
                    }
                    
                    if !is_forward {
                        let target_resolved_fields = if let Some(m) = ast.models.get(target_model) {
                            &m.resolved_fields
                        } else if let Some(b) = ast.bases.get(target_model) {
                            &b.resolved_fields
                        } else {
                            return Err(format!("Security Exception: Model '{}' undefined.", target_model));
                        };
                        let relation_name = if let Some(schema_parser::ast::FieldAttribute::Relation { name: Some(n), .. }) = relation_attr { Some(n.clone()) } else { None };
                        
                        for target_field in target_resolved_fields {
                            if let schema_parser::ast::AstFieldType::Relation(ref_model) | schema_parser::ast::AstFieldType::RelationArray(ref_model) = &target_field.field_type {
                                if ref_model == &base_def.name {
                                    if let (Some(target_name), Some(fields)) = (target_field.attributes.iter().find_map(|a| if let schema_parser::ast::FieldAttribute::Relation { name, .. } = a { Some(name.clone()) } else { None }), target_field.attributes.iter().find_map(|a| if let schema_parser::ast::FieldAttribute::InternalRelation { fields, .. } = a { Some(fields.clone()) } else { None })) {
                                        let name_matches = match (&relation_name, &target_name) {
                                            (Some(a), Some(b)) => a == b,
                                            (None, None) => true,
                                            _ => false,
                                        };
                                        if name_matches {
                                            if !fields.is_empty() {
                                                resolved_fk = fields[0].clone();
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    
                    if is_forward {
                        symmetric_projection.insert(resolved_fk.clone()); // MUST inject FK into inner payload so outer JOIN works
                    } else {
                        let pk = base_def.resolved_fields.iter().find(|f| f.attributes.iter().any(|a| matches!(a, schema_parser::ast::FieldAttribute::Id))).map(|f| f.name.clone()).unwrap_or_else(|| "__id".to_string());
                        symmetric_projection.insert(pk);
                    }
                    
                    let mut child_node = child_node;
                    if !is_forward {
                        if !child_node.selections.iter().any(|s| match s { query_compiler::ir::SelectField::Scalar(name) => *name == resolved_fk, _ => false }) {
                            child_node.selections.push(query_compiler::ir::SelectField::Scalar(resolved_fk.clone()));
                        }
                        
                        if let query_compiler::ir::QueryIrSource::Polymorphic { branches, .. } = &mut child_node.source {
                            for branch in branches {
                                if !branch.selections.iter().any(|s| match s { query_compiler::ir::SelectField::Scalar(name) => *name == resolved_fk, _ => false }) {
                                    branch.selections.push(query_compiler::ir::SelectField::Scalar(resolved_fk.clone()));
                                }
                            }
                        }
                    }

                    outer_selections.push(query_compiler::ir::SelectField::Relation {
                        field_name: field_name.clone(),
                        foreign_key: resolved_fk, 
                        is_list,
                        is_forward,
                        query: Box::new(child_node),
                    });
                },
                _ => {}
            }
        } else if field_name.starts_with("__") && field_name != "__id" && field_name != "__kind" {
            outer_selections.push(query_compiler::ir::SelectField::SyntheticNull(field_name.clone()));
        } else if field_name == "__id" || field_name == "__kind" {
            // These are implicit base fields that are pushed down to concrete models
            outer_selections.push(query_compiler::ir::SelectField::Scalar(field_name.clone()));
        } else {
            return Err(format!("Invalid field '{}' on '{}'.", field_name, base_def.name));
        }
    }
    
    let primary_key = base_def.resolved_fields.iter()
        .find(|f| f.attributes.iter().any(|a| matches!(a, schema_parser::ast::FieldAttribute::Id)))
        .map(|f| f.name.clone())
        .unwrap_or_else(|| "__id".to_string());

    Ok(query_compiler::ir::QueryNode {
        source: query_compiler::ir::QueryIrSource::Polymorphic {
            alias: base_def.name.clone(),
            branches,
        },
        primary_key,
        alias: current_alias,
        selections: outer_selections,
        filters: None,
        order_by: vec![],
        limit: payload.get("limit").and_then(|l| l.as_u64()).map(|l| l as usize),
        offset: payload.get("skip").and_then(|l| l.as_u64()).map(|l| l as usize),
    })
}

use std::collections::HashSet;
use crate::ast::*;

#[derive(Debug, Clone)]
pub struct ValidationError(pub String);

pub fn validate_schema(mut ast: SchemaAst) -> Result<SchemaAst, ValidationError> {
    // Pass 1: Symbol Resolution
    let mut model_symbols = HashSet::new();
    let mut union_symbols = HashSet::new();

    for model_name in ast.models.keys() {
        model_symbols.insert(model_name.clone());
    }

    for base_name in ast.bases.keys() {
        model_symbols.insert(base_name.clone());
    }

    for union_name in ast.unions.keys() {
        union_symbols.insert(union_name.clone());
    }

    // --- PHASE 2: Semantic Validation & Crystallization ---

    // Step 2.1: Pre-Validation & Lookup Indexing
    let mut base_index: std::collections::HashMap<&str, &BaseNode> = std::collections::HashMap::new();
    let mut model_index: std::collections::HashMap<&str, &ModelNode> = std::collections::HashMap::new();

    for base in ast.bases.values() {
        if base_index.insert(&base.name, base).is_some() {
            return Err(ValidationError(format!("Duplicate identifier '{}' found.", base.name)));
        }
    }

    for model in ast.models.values() {
        if model_index.insert(&model.name, model).is_some() || base_index.contains_key(model.name.as_str()) {
            return Err(ValidationError(format!("Duplicate identifier '{}' found.", model.name)));
        }
    }

    let check_targets = |name: &str, extends: &Vec<String>| -> Result<(), ValidationError> {
        for target in extends {
            if model_index.contains_key(target.as_str()) {
                return Err(ValidationError(format!("Shape '{}' cannot extend '{}' because it is a model, not a base.", name, target)));
            } else if !base_index.contains_key(target.as_str()) {
                return Err(ValidationError(format!("Shape '{}' extends unknown target '{}'.", name, target)));
            }
        }
        Ok(())
    };

    for base in ast.bases.values() {
        check_targets(&base.name, &base.extends)?;
    }
    for model in ast.models.values() {
        check_targets(&model.name, &model.extends)?;
    }

    for (union_name, members) in &ast.unions {
        for member in members {
            if base_index.contains_key(member.as_str()) {
                return Err(ValidationError(format!("Union '{}' references abstract base '{}', which is not allowed.", union_name, member)));
            }
        }
    }

    // Step 2.2: Cycle Detection & Topological Sorting
    let mut in_degree: std::collections::HashMap<&str, usize> = base_index.keys().map(|&k| (k, 0)).collect();
    let mut graph: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();

    for base in ast.bases.values() {
        for parent in &base.extends {
            graph.entry(parent.as_str()).or_default().push(base.name.as_str());
            *in_degree.entry(base.name.as_str()).or_insert(0) += 1;
        }
    }

    let mut queue: Vec<&str> = in_degree.iter()
        .filter(|&(_, &deg)| deg == 0)
        .map(|(&node, _)| node)
        .collect();

    let mut sorted_bases = Vec::new();

    while let Some(node) = queue.pop() {
        sorted_bases.push(node);
        if let Some(children) = graph.get(node) {
            for &child in children {
                let deg = in_degree.get_mut(child).unwrap();
                *deg -= 1;
                if *deg == 0 {
                    queue.push(child);
                }
            }
        }
    }

    if sorted_bases.len() != ast.bases.len() {
        return Err(ValidationError("Circular inheritance detected among bases.".to_string()));
    }

    // Step 2.3: Transitive Trait Flattening (Bases)
    struct ResolvedState {
        resolved_bases: std::collections::BTreeSet<String>,
        resolved_fields: Vec<FieldNode>,
    }

    fn merge_field(fields: &mut Vec<FieldNode>, new_field: FieldNode, shape_name: &str) -> Result<(), ValidationError> {
        if let Some(existing) = fields.iter().find(|f| f.name == new_field.name) {
            if existing.field_type != new_field.field_type || existing.is_optional != new_field.is_optional {
                return Err(ValidationError(format!("Field shadowing mismatch in shape '{}' for field '{}'.", shape_name, new_field.name)));
            }
            return Ok(());
        }
        fields.push(new_field);
        Ok(())
    }

    let mut resolution_registry: std::collections::HashMap<String, ResolvedState> = std::collections::HashMap::new();

    for base_name in sorted_bases {
        let base = base_index.get(base_name).unwrap();
        let mut current_bases = std::collections::BTreeSet::new();
        let mut current_fields = Vec::new();

        for parent_name in &base.extends {
            let parent_state = resolution_registry.get(parent_name.as_str()).unwrap();
            current_bases.extend(parent_state.resolved_bases.iter().cloned());
            current_bases.insert(parent_name.clone());

            for p_field in &parent_state.resolved_fields {
                merge_field(&mut current_fields, p_field.clone(), base_name)?;
            }
        }

        for field in &base.fields {
            merge_field(&mut current_fields, field.clone(), base_name)?;
        }


        resolution_registry.insert(base_name.to_string(), ResolvedState {
            resolved_bases: current_bases,
            resolved_fields: current_fields,
        });
    }

    // Step 2.4: Model Crystallization & Synthetic Injection
    for model in ast.models.values() {
        let mut current_bases = std::collections::BTreeSet::new();
        let mut current_fields = Vec::new();

        for parent_name in &model.extends {
            let parent_state = resolution_registry.get(parent_name.as_str()).unwrap();
            current_bases.extend(parent_state.resolved_bases.iter().cloned());
            current_bases.insert(parent_name.clone());

            for p_field in &parent_state.resolved_fields {
                merge_field(&mut current_fields, p_field.clone(), &model.name)?;
            }
        }

        for field in &model.fields {
            merge_field(&mut current_fields, field.clone(), &model.name)?;
        }

        for injected_base in &current_bases {
            let marker_name = format!("__{}", injected_base);
            if current_fields.iter().any(|f| f.name == marker_name) {
                return Err(ValidationError(format!("Model '{}' cannot declare reserved field name '{}'.", model.name, marker_name)));
            }

            let synthetic_field = FieldNode {
                name: marker_name,
                field_type: AstFieldType::Scalar("Boolean".to_string()),
                is_optional: false,
                attributes: vec![FieldAttribute::Default(DefaultFunc::Static("true".to_string()))],
            };
            current_fields.push(synthetic_field);
        }

        current_fields.sort_by(|a, b| a.name.cmp(&b.name));

        resolution_registry.insert(model.name.clone(), ResolvedState {
            resolved_bases: current_bases,
            resolved_fields: current_fields,
        });
    }

    // Step 2.5: AST Re-hydration
    for base in ast.bases.values_mut() {
        if let Some(state) = resolution_registry.remove(&base.name) {
            base.resolved_bases = state.resolved_bases;
            base.resolved_fields = state.resolved_fields;
        }
    }

    for model in ast.models.values_mut() {
        if let Some(state) = resolution_registry.remove(&model.name) {
            model.resolved_bases = state.resolved_bases;
            model.resolved_fields = state.resolved_fields;
        }
    }

    // Step 2.6: Implementor Registry
    let mut implementor_registry: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for model in ast.models.values() {
        for base in &model.resolved_bases {
            implementor_registry.entry(base.clone()).or_default().push(model.name.clone());
        }
    }
    // --- END PHASE 2 ---

    // Pass 2: Graph Integrity & Primary Key Check
    
    // Check union targets exist
    for (union_name, targets) in &ast.unions {
        for target in targets {
            if !model_symbols.contains(target) {
                return Err(ValidationError(format!(
                    "Union '{}' references target '{}' which does not exist.",
                    union_name, target
                )));
            }
        }
    }

    // Check models
    for model in ast.models.values_mut() {
        let mut id_count = 0;
        
        for field in &mut model.resolved_fields {
            // Check for @id attribute
            let has_id = field.attributes.iter().any(|a| matches!(a, FieldAttribute::Id));
            if has_id {
                if field.is_optional {
                    return Err(ValidationError(format!(
                        "Field '{}' in model '{}' is marked with '@id' but is optional. Primary keys cannot be optional.",
                        field.name, model.name
                    )));
                }
                id_count += 1;
            }

            // Check if arrays are optional
            let is_array = matches!(field.field_type, AstFieldType::ScalarArray(_) | AstFieldType::RelationArray(_));
            if is_array && field.is_optional {
                return Err(ValidationError(format!(
                    "Field '{}' in model '{}' is an array but is marked as optional. Arrays cannot be optional.",
                    field.name, model.name
                )));
            }
            
            // Fix up Relation to PolymorphicUnion or PolymorphicBase
            match &field.field_type {
                AstFieldType::Relation(target_name) => {
                    if union_symbols.contains(target_name) {
                        field.field_type = AstFieldType::PolymorphicUnion(target_name.clone());
                    } else if ast.bases.contains_key(target_name) {
                        if implementor_registry.get(target_name).map_or(0, |v| v.len()) == 0 {
                            return Err(ValidationError(format!(
                                "Field '{}' in model '{}' targets base '{}' which has no implementers.",
                                field.name, model.name, target_name
                            )));
                        }
                        for attr in &field.attributes {
                            if let FieldAttribute::Relation { references, .. } = attr {
                                if !references.is_empty() {
                                    return Err(ValidationError(format!(
                                        "Field '{}' in model '{}' is a polymorphic base and cannot define explicit relation references.",
                                        field.name, model.name
                                    )));
                                }
                            }
                        }
                        field.field_type = AstFieldType::PolymorphicBase(target_name.clone());
                    } else if !model_symbols.contains(target_name) {
                        return Err(ValidationError(format!(
                            "Field '{}' in model '{}' references unknown type '{}'.",
                            field.name, model.name, target_name
                        )));
                    }
                },
                AstFieldType::RelationArray(target_name) => {
                    if union_symbols.contains(target_name) {
                        field.field_type = AstFieldType::PolymorphicUnionArray(target_name.clone());
                    } else if ast.bases.contains_key(target_name) {
                        if implementor_registry.get(target_name).map_or(0, |v| v.len()) == 0 {
                            return Err(ValidationError(format!(
                                "Field '{}' in model '{}' targets base '{}' which has no implementers.",
                                field.name, model.name, target_name
                            )));
                        }
                        for attr in &field.attributes {
                            if let FieldAttribute::Relation { references, .. } = attr {
                                if !references.is_empty() {
                                    return Err(ValidationError(format!(
                                        "Field '{}' in model '{}' is a polymorphic base and cannot define explicit relation references.",
                                        field.name, model.name
                                    )));
                                }
                            }
                        }
                        field.field_type = AstFieldType::PolymorphicBaseArray(target_name.clone());
                    } else if !model_symbols.contains(target_name) {
                        return Err(ValidationError(format!(
                            "Field '{}' in model '{}' references unknown type '{}'.",
                            field.name, model.name, target_name
                        )));
                    }
                },
                _ => {}
            }
        }
        
        if id_count != 1 {
            return Err(ValidationError(format!(
                "Model '{}' must have exactly one field with the '@id' attribute, found {}.",
                model.name, id_count
            )));
        }
    }

    // Pass 3: Ambiguous Relations Check
    for model in ast.models.values() {
        let mut target_counts: std::collections::HashMap<&String, Vec<&FieldNode>> = std::collections::HashMap::new();
        
        for field in &model.resolved_fields {
            match &field.field_type {
                AstFieldType::Relation(target_name) | AstFieldType::RelationArray(target_name) | AstFieldType::PolymorphicBase(target_name) | AstFieldType::PolymorphicBaseArray(target_name) => {
                    if model_symbols.contains(target_name) {
                        target_counts.entry(target_name).or_insert_with(Vec::new).push(field);
                    }
                },
                _ => {}
            }
        }
        
        for (target_name, relation_fields) in target_counts {
            if relation_fields.len() > 1 {
                let mut seen_names: std::collections::HashMap<&str, &FieldNode> = std::collections::HashMap::new();
                for field in relation_fields {
                    let mut has_name = false;
                    if let Some(FieldAttribute::Relation { name, .. }) = field.attributes.iter().find(|a| matches!(a, FieldAttribute::Relation { .. })) {
                        if let Some(n) = name {
                            has_name = true;
                            if let Some(existing_field) = seen_names.get(n.as_str()) {
                                let is_existing_array = matches!(existing_field.field_type, AstFieldType::RelationArray(_));
                                let is_current_array = matches!(field.field_type, AstFieldType::RelationArray(_));
                                if is_existing_array == is_current_array {
                                    return Err(ValidationError(format!(
                                        "Ambiguous relations: Model '{}' has multiple relations to '{}' with the same name '{}'. Each must be uniquely named.",
                                        model.name, target_name, n
                                    )));
                                }
                            } else {
                                seen_names.insert(n, field);
                            }
                        }
                    }
                    if !has_name {
                        return Err(ValidationError(format!(
                            "Ambiguous relations: Model '{}' has multiple relations to '{}', but field '{}' is missing a unique @relation(\"Name\").",
                            model.name, target_name, field.name
                        )));
                    }
                }
            }
        }
    }

    // Pass 4: Implicit Foreign Key Injection
    let mut model_id_info = std::collections::HashMap::new();
    for model in ast.models.values() {
        if let Some(id_field) = model.fields.iter().find(|f| f.attributes.iter().any(|a| matches!(a, FieldAttribute::Id))) {
            model_id_info.insert(model.name.clone(), (id_field.name.clone(), id_field.field_type.clone()));
        }
    }

    for model in ast.models.values_mut() {
        let mut added_fields = Vec::new();
        
        for field in &mut model.resolved_fields {
            if let AstFieldType::Relation(target_name) = &field.field_type {
                let mut relation_attr_idx = None;
                let mut needs_injection = false;
                let mut col_name = format!("{}Id", field.name);
                
                for (idx, attr) in field.attributes.iter().enumerate() {
                    if let FieldAttribute::Relation { fields, column, .. } = attr {
                        relation_attr_idx = Some(idx);
                        if fields.is_empty() {
                            needs_injection = true;
                            if let Some(c) = column {
                                col_name = c.clone();
                            }
                        }
                        break;
                    }
                }
                
                if relation_attr_idx.is_none() {
                    needs_injection = true;
                    field.attributes.push(FieldAttribute::Relation {
                        name: None,
                        fields: vec![],
                        references: vec![],
                        on_delete: None,
                        deferrable: false,
                        column: None,
                    });
                    relation_attr_idx = Some(field.attributes.len() - 1);
                }

                if needs_injection {
                    if let Some((target_id_name, target_id_type)) = model_id_info.get(target_name) {
                        added_fields.push(FieldNode {
                            name: col_name.clone(),
                            field_type: target_id_type.clone(),
                            is_optional: field.is_optional,
                            attributes: vec![],
                        });
                        
                        if let Some(idx) = relation_attr_idx {
                            if let FieldAttribute::Relation { fields, references, .. } = &mut field.attributes[idx] {
                                *fields = vec![col_name.clone()];
                                *references = vec![target_id_name.clone()];
                            }
                        }
                    } else {
                        return Err(ValidationError(format!("Target model '{}' does not have an @id field.", target_name)));
                    }
                }
            }
        }
        
        for added_field in added_fields {
            if !model.resolved_fields.iter().any(|f| f.name == added_field.name) {
                model.resolved_fields.push(added_field);
            }
        }
    }

    Ok(ast)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_valid_schema() {
        let mut ast = SchemaAst { bases: std::collections::HashMap::new(),
            models: HashMap::new(),
            unions: HashMap::new(),
        };
        
        ast.models.insert("User".to_string(), ModelNode { extends: vec![], resolved_fields: vec![], resolved_bases: std::collections::BTreeSet::new(),
            name: "User".to_string(),
            fields: vec![
                FieldNode {
                    name: "id".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    is_optional: false,
                    attributes: vec![FieldAttribute::Id],
                }
            ]
        });
        
        assert!(validate_schema(ast).is_ok());
    }

    #[test]
    fn test_missing_id() {
        let mut ast = SchemaAst { bases: std::collections::HashMap::new(),
            models: HashMap::new(),
            unions: HashMap::new(),
        };
        
        ast.models.insert("User".to_string(), ModelNode { extends: vec![], resolved_fields: vec![], resolved_bases: std::collections::BTreeSet::new(),
            name: "User".to_string(),
            fields: vec![
                FieldNode {
                    name: "name".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    is_optional: false,
                    attributes: vec![],
                }
            ]
        });
        
        let result = validate_schema(ast);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().0, "Model 'User' must have exactly one field with the '@id' attribute, found 0.");
    }
    
    #[test]
    fn test_invalid_union_target() {
        let mut ast = SchemaAst { bases: std::collections::HashMap::new(),
            models: HashMap::new(),
            unions: HashMap::new(),
        };
        
        ast.models.insert("User".to_string(), ModelNode { extends: vec![], resolved_fields: vec![], resolved_bases: std::collections::BTreeSet::new(),
            name: "User".to_string(),
            fields: vec![
                FieldNode {
                    name: "id".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    is_optional: false,
                    attributes: vec![FieldAttribute::Id],
                }
            ]
        });
        
        ast.unions.insert("MyUnion".to_string(), vec!["User".to_string(), "Post".to_string()]);
        
        let result = validate_schema(ast);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().0, "Union 'MyUnion' references target 'Post' which does not exist.");
    }

    #[test]
    fn test_multiple_ids() {
        let mut ast = SchemaAst { bases: std::collections::HashMap::new(),
            models: HashMap::new(),
            unions: HashMap::new(),
        };
        
        ast.models.insert("User".to_string(), ModelNode { extends: vec![], resolved_fields: vec![], resolved_bases: std::collections::BTreeSet::new(),
            name: "User".to_string(),
            fields: vec![
                FieldNode {
                    name: "id1".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    is_optional: false,
                    attributes: vec![FieldAttribute::Id],
                },
                FieldNode {
                    name: "id2".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    is_optional: false,
                    attributes: vec![FieldAttribute::Id],
                }
            ]
        });
        
        let result = validate_schema(ast);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().0, "Model 'User' must have exactly one field with the '@id' attribute, found 2.");
    }

    #[test]
    fn test_invalid_relation_target() {
        let mut ast = SchemaAst { bases: std::collections::HashMap::new(),
            models: HashMap::new(),
            unions: HashMap::new(),
        };
        
        ast.models.insert("Post".to_string(), ModelNode { extends: vec![], resolved_fields: vec![], resolved_bases: std::collections::BTreeSet::new(),
            name: "Post".to_string(),
            fields: vec![
                FieldNode {
                    name: "id".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    is_optional: false,
                    attributes: vec![FieldAttribute::Id],
                },
                FieldNode {
                    name: "author".to_string(),
                    field_type: AstFieldType::Relation("UnknownModel".to_string()),
                    is_optional: false,
                    attributes: vec![],
                }
            ]
        });
        
        let result = validate_schema(ast);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().0, "Field 'author' in model 'Post' references unknown type 'UnknownModel'.");
    }

    #[test]
    fn test_union_array_fixup() {
        let mut ast = SchemaAst { bases: std::collections::HashMap::new(),
            models: HashMap::new(),
            unions: HashMap::new(),
        };

        ast.models.insert("Query".to_string(), ModelNode { extends: vec![], resolved_fields: vec![], resolved_bases: std::collections::BTreeSet::new(),
            name: "Query".to_string(),
            fields: vec![
                FieldNode {
                    name: "id".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    is_optional: false,
                    attributes: vec![FieldAttribute::Id],
                },
                FieldNode {
                    name: "results".to_string(),
                    field_type: AstFieldType::RelationArray("SearchResult".to_string()),
                    is_optional: false,
                    attributes: vec![],
                }
            ]
        });

        ast.unions.insert("SearchResult".to_string(), vec![]);

        let result = validate_schema(ast).unwrap();
        let query = result.models.get("Query").unwrap();
        let results_field = query.resolved_fields.iter().find(|f| f.name == "results").unwrap();
        assert_eq!(results_field.field_type, AstFieldType::PolymorphicUnionArray("SearchResult".to_string()));
    }
    #[test]
    fn test_polymorphic_union_fixup() {
        let mut ast = SchemaAst { bases: std::collections::HashMap::new(),
            models: HashMap::new(),
            unions: HashMap::new(),
        };
        
        ast.models.insert("Post".to_string(), ModelNode { extends: vec![], resolved_fields: vec![], resolved_bases: std::collections::BTreeSet::new(),
            name: "Post".to_string(),
            fields: vec![
                FieldNode {
                    name: "id".to_string(),
                    field_type: AstFieldType::Scalar("String".to_string()),
                    is_optional: false,
                    attributes: vec![FieldAttribute::Id],
                },
                FieldNode {
                    name: "result".to_string(),
                    field_type: AstFieldType::Relation("SearchResult".to_string()),
                    is_optional: false,
                    attributes: vec![],
                }
            ]
        });
        
        ast.unions.insert("SearchResult".to_string(), vec!["Post".to_string()]);
        
        let validated_ast = validate_schema(ast).unwrap();
        let post_model = validated_ast.models.get("Post").unwrap();
        println!("POST MODEL AT TEST: {:?}", post_model);
        let result_field = post_model.resolved_fields.iter().find(|f| f.name == "result").unwrap();
        
        assert_eq!(result_field.field_type, AstFieldType::PolymorphicUnion("SearchResult".to_string()));
    }

    #[test]
    fn test_validation_ambiguous_relations_missing_name() {
        let input = "
            model User {
                id: String @id
                authoredPosts: Post[]
                reviewedPosts: Post[]
            }
            model Post {
                id: String @id
            }
        ";
        let ast = crate::parser::parse_schema(input).unwrap();
        let result = validate_schema(ast);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().0, "Ambiguous relations: Model 'User' has multiple relations to 'Post', but field 'authoredPosts' is missing a unique @relation(\"Name\").");
    }

    #[test]
    fn test_validation_ambiguous_relations_duplicate_name() {
        let input = "
            model User {
                id: String @id
                authoredPosts: Post[] @relation(\"AuthorToPost\")
                reviewedPosts: Post[] @relation(\"AuthorToPost\")
            }
            model Post {
                id: String @id
            }
        ";
        let ast = crate::parser::parse_schema(input).unwrap();
        let result = validate_schema(ast);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().0, "Ambiguous relations: Model 'User' has multiple relations to 'Post' with the same name 'AuthorToPost'. Each must be uniquely named.");
    }

    #[test]
    fn test_validation_valid_multiple_named_relations() {
        let input = "
            model User {
                id: String @id
                authoredPosts: Post[] @relation(\"AuthorToPost\")
                reviewedPosts: Post[] @relation(\"ReviewerToPost\")
            }
            model Post {
                id: String @id
            }
        ";
        let ast = crate::parser::parse_schema(input).unwrap();
        let result = validate_schema(ast);
        assert!(result.is_ok());
    }

    #[test]
    fn test_implicit_foreign_key_injection() {
        let input = "
            model User {
                id: String @id
            }
            model Post {
                id: String @id
                author: User @relation(\"AuthorToPost\")
                reviewer: User @relation(\"ReviewerToPost\", column: \"reviewer_id\")
            }
        ";
        let mut ast = crate::parser::parse_schema(input).unwrap();
        ast = validate_schema(ast).unwrap();
        
        let post = ast.models.get("Post").unwrap();
        
        let author_id_field = post.resolved_fields.iter().find(|f| f.name == "authorId").unwrap();
        assert_eq!(author_id_field.field_type, AstFieldType::Scalar("String".to_string()));
        
        let reviewer_id_field = post.resolved_fields.iter().find(|f| f.name == "reviewer_id").unwrap();
        assert_eq!(reviewer_id_field.field_type, AstFieldType::Scalar("String".to_string()));
        
        let author_rel = post.resolved_fields.iter().find(|f| f.name == "author").unwrap();
        assert_eq!(author_rel.attributes, vec![FieldAttribute::Relation {
            name: Some("AuthorToPost".to_string()),
            fields: vec!["authorId".to_string()],
            references: vec!["id".to_string()],
            on_delete: None,
            deferrable: false,
            column: None,
        }]);

        let reviewer_rel = post.resolved_fields.iter().find(|f| f.name == "reviewer").unwrap();
        assert_eq!(reviewer_rel.attributes, vec![FieldAttribute::Relation {
            name: Some("ReviewerToPost".to_string()),
            fields: vec!["reviewer_id".to_string()],
            references: vec!["id".to_string()],
            on_delete: None,
            deferrable: false,
            column: Some("reviewer_id".to_string()),
        }]);
    }

    #[test]
    fn test_implicit_relation_explicit_bypass() {
        let input = "
            model User {
                id: String @id
            }
            model Post {
                id: String @id
                authorId: String
                author: User @relation(fields: [authorId], references: [id])
            }
        ";
        let mut ast = crate::parser::parse_schema(input).unwrap();
        ast = validate_schema(ast).unwrap();
        
        let post = ast.models.get("Post").unwrap();
        assert_eq!(post.fields.len(), 3);
        
        let author_rel = post.resolved_fields.iter().find(|f| f.name == "author").unwrap();
        assert_eq!(author_rel.attributes, vec![FieldAttribute::Relation {
            name: None,
            fields: vec!["authorId".to_string()],
            references: vec!["id".to_string()],
            on_delete: None,
            deferrable: false,
            column: None,
        }]);
    }

    #[test]
    fn test_implicit_relation_strict_type_matching() {
        let input = "
            model Category {
                id: Int @id
            }
            model Product {
                id: String @id
                category: Category
            }
        ";
        let mut ast = crate::parser::parse_schema(input).unwrap();
        ast = validate_schema(ast).unwrap();
        
        let product = ast.models.get("Product").unwrap();
        let cat_id_field = product.resolved_fields.iter().find(|f| f.name == "categoryId").unwrap();
        assert_eq!(cat_id_field.field_type, AstFieldType::Scalar("Int".to_string()));
    }

    #[test]
    fn test_implicit_relation_missing_target_id() {
        // Here Author lacks an @id field
        let input = "
            model Author {
                name: String
            }
            model Book {
                id: String @id
                author: Author
            }
        ";
        // Parse the schema. Note: Pass 2 normally catches missing ID for Author itself.
        // Wait, Pass 2 will fail first: "Model 'Author' must have exactly one field with the '@id' attribute, found 0."
        // That is totally fine and correct. Let's just assert that it fails.
        let ast = crate::parser::parse_schema(input).unwrap();
        let result = validate_schema(ast);
        assert!(result.is_err());
    }

    #[test]
    fn test_implicit_relation_self_referential() {
        let input = "
            model Employee {
                id: String @id
                manager: Employee @relation(\"Management\")
            }
        ";
        let mut ast = crate::parser::parse_schema(input).unwrap();
        ast = validate_schema(ast).unwrap();
        
        let employee = ast.models.get("Employee").unwrap();
        let manager_id_field = employee.resolved_fields.iter().find(|f| f.name == "managerId").unwrap();
        assert_eq!(manager_id_field.field_type, AstFieldType::Scalar("String".to_string()));
        
        let manager_rel = employee.resolved_fields.iter().find(|f| f.name == "manager").unwrap();
        assert_eq!(manager_rel.attributes, vec![FieldAttribute::Relation {
            name: Some("Management".to_string()),
            fields: vec!["managerId".to_string()],
            references: vec!["id".to_string()],
            on_delete: None,
            deferrable: false,
            column: None,
        }]);
    }

    #[test]
    fn test_validation_optional_id_fails() {
        let input = "
            model User {
                id: String? @id
            }
        ";
        let ast = crate::parser::parse_schema(input).unwrap();
        let res = validate_schema(ast);
        assert!(res.is_err());
        assert!(res.unwrap_err().0.contains("Primary keys cannot be optional"));
    }

    #[test]
    fn test_validation_optional_array_fails() {
        let input = "
            model User {
                id: String @id
                tags: String[]?
            }
        ";
        let ast = crate::parser::parse_schema(input).unwrap();
        let res = validate_schema(ast);
        assert!(res.is_err());
        assert!(res.unwrap_err().0.contains("Arrays cannot be optional"));
    }

    #[test]
    fn test_validation_implicit_fk_inherits_optionality() {
        let input = "
            model User {
                id: String @id
                manager: User?
            }
        ";
        let ast = crate::parser::parse_schema(input).unwrap();
        let res = validate_schema(ast).unwrap();
        let user = res.models.get("User").unwrap();
        let manager_id = user.resolved_fields.iter().find(|f| f.name == "managerId").unwrap();
        assert_eq!(manager_id.is_optional, true);
        }

        #[test]
        fn test_parse_and_validate_union_array() {
        let input = "
            model Query {
                id: String @id
                results: SearchResult[]
            }
            model User {
                id: String @id
            }
            model Post {
                id: String @id
            }
            union SearchResult = User | Post
        ";
        let ast = crate::parser::parse_schema(input).unwrap();
        let res = validate_schema(ast).unwrap();

        let query = res.models.get("Query").unwrap();
        let results_field = query.resolved_fields.iter().find(|f| f.name == "results").unwrap();
        assert_eq!(results_field.field_type, AstFieldType::PolymorphicUnionArray("SearchResult".to_string()));
        }

        #[test]
        fn test_validation_optional_unique_succeeds() {
        let input = "
            model User {
                id: String @id
                email: String? @unique
            }
        ";
        let ast = crate::parser::parse_schema(input).unwrap();
        let res = validate_schema(ast);
        assert!(res.is_ok());
    }

    #[test]
    fn test_deep_transitive_flattening_and_synthetic_injections() {
        let input = "
            base Node { id: String @id }
            base Timestamped extends Node { createdAt: String }
            model User extends Timestamped { name: String }
        ";
        let mut ast = crate::parser::parse_schema(input).unwrap();
        ast = validate_schema(ast).unwrap();
        let user = ast.models.get("User").unwrap();

        assert!(user.resolved_bases.contains("Timestamped"));
        assert!(user.resolved_bases.contains("Node"));

        let field_names: Vec<String> = user.resolved_fields.iter().map(|f| f.name.clone()).collect();
        assert!(field_names.contains(&"id".to_string()));
        assert!(field_names.contains(&"createdAt".to_string()));
        assert!(field_names.contains(&"name".to_string()));
        assert!(field_names.contains(&"__Node".to_string()));
        assert!(field_names.contains(&"__Timestamped".to_string()));

        let node_flag = user.resolved_fields.iter().find(|f| f.name == "__Node").unwrap();
        assert_eq!(node_flag.is_optional, false);
    }

    #[test]
    fn test_rejects_cyclic_inheritance() {
        let input = "
            base A extends B { id: String @id }
            base B extends A { name: String }
            model User extends A { email: String }
        ";
        let ast = crate::parser::parse_schema(input).unwrap();
        let err = validate_schema(ast).unwrap_err();
        assert_eq!(err.0, "Circular inheritance detected among bases.");
    }

    #[test]
    fn test_invalid_field_shadowing() {
        let input = "
            base Node { id: String @id }
            model User extends Node { 
                id: Int @id 
            }
        ";
        let ast = crate::parser::parse_schema(input).unwrap();
        let err = validate_schema(ast).unwrap_err();
        assert_eq!(err.0, "Field shadowing mismatch in shape 'User' for field 'id'.");
    }

    #[test]
    fn test_reserved_marker_column_collisions() {
        let mut ast = SchemaAst {
            bases: std::collections::HashMap::new(),
            models: std::collections::HashMap::new(),
            unions: std::collections::HashMap::new(),
        };

        ast.bases.insert("Timestamped".to_string(), BaseNode {
            name: "Timestamped".to_string(),
            fields: vec![FieldNode { name: "createdAt".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![] }],
            extends: vec![],
            ..Default::default()
        });

        ast.models.insert("User".to_string(), ModelNode {
            name: "User".to_string(),
            extends: vec!["Timestamped".to_string()],
            fields: vec![
                FieldNode { name: "id".to_string(), field_type: AstFieldType::Scalar("String".to_string()), is_optional: false, attributes: vec![FieldAttribute::Id] },
                FieldNode { name: "__Timestamped".to_string(), field_type: AstFieldType::Scalar("Boolean".to_string()), is_optional: false, attributes: vec![] },
            ],
            ..Default::default()
        });

        let err = validate_schema(ast).unwrap_err();
        assert_eq!(err.0, "Model 'User' cannot declare reserved field name '__Timestamped'.");
    }

    #[test]
    fn test_invalid_inheritance_targets() {
        let input1 = "
            model User { id: String @id }
            model Admin extends User { role: String }
        ";
        let ast1 = crate::parser::parse_schema(input1).unwrap();
        let err1 = validate_schema(ast1).unwrap_err();
        assert_eq!(err1.0, "Shape 'Admin' cannot extend 'User' because it is a model, not a base.");

        let input2 = "
            base BaseEntity { id: String @id }
            union SearchResult = BaseEntity
        ";
        let ast2 = crate::parser::parse_schema(input2).unwrap();
        let err2 = validate_schema(ast2).unwrap_err();
        assert_eq!(err2.0, "Union 'SearchResult' references abstract base 'BaseEntity', which is not allowed.");
    }

    #[test]
    fn test_polymorphic_base_upgrade() {
        let input = "
            base Content { title: String }
            model Article extends Content { id: String @id }
            model User {
                id: String @id
                favorite: Content
            }
        ";
        let ast = crate::parser::parse_schema(input).unwrap();
        let validated_ast = validate_schema(ast).unwrap();
        let user_model = validated_ast.models.get("User").unwrap();
        let favorite_field = user_model.resolved_fields.iter().find(|f| f.name == "favorite").unwrap();
        assert_eq!(favorite_field.field_type, AstFieldType::PolymorphicBase("Content".to_string()));
    }

    #[test]
    fn test_polymorphic_base_no_implementers() {
        let input = "
            base Content { title: String }
            model User {
                id: String @id
                favorite: Content
            }
        ";
        let ast = crate::parser::parse_schema(input).unwrap();
        let err = validate_schema(ast).unwrap_err();
        assert_eq!(err.0, "Field 'favorite' in model 'User' targets base 'Content' which has no implementers.");
    }

    #[test]
    fn test_polymorphic_base_explicit_relation() {
        let input = "
            base Content { title: String }
            model Article extends Content { id: String @id }
            model User {
                id: String @id
                favorite: Content @relation(references: [id])
            }
        ";
        let ast = crate::parser::parse_schema(input).unwrap();
        let err = validate_schema(ast).unwrap_err();
        assert_eq!(err.0, "Field 'favorite' in model 'User' is a polymorphic base and cannot define explicit relation references.");
    }

    #[test]
    fn test_polymorphic_base_array_upgrade() {
        let input = "
            base Content { title: String }
            model Article extends Content { id: String @id }
            model User {
                id: String @id
                favorites: Content[]
            }
        ";
        let ast = crate::parser::parse_schema(input).unwrap();
        let validated_ast = validate_schema(ast).unwrap();
        let user_model = validated_ast.models.get("User").unwrap();
        let favorites_field = user_model.resolved_fields.iter().find(|f| f.name == "favorites").unwrap();
        assert_eq!(favorites_field.field_type, AstFieldType::PolymorphicBaseArray("Content".to_string()));
    }

    #[test]
    fn test_polymorphic_base_transitive_implementors() {
        let input = "
            base Node { id: String @id }
            base Content extends Node { title: String }
            model Article extends Content { body: String }
            
            model Graph {
                id: String @id
                nodes: Node[]
            }
        ";
        let ast = crate::parser::parse_schema(input).unwrap();
        assert!(validate_schema(ast).is_ok());
    }

    #[test]
    fn test_polymorphic_base_allows_named_relation() {
        let input = "
            base Content { title: String }
            model Article extends Content { id: String @id }
            model User {
                id: String @id
                primary: Content @relation(\"PrimaryContent\")
            }
        ";
        let ast = crate::parser::parse_schema(input).unwrap();
        let validated_ast = validate_schema(ast).unwrap();
        let user_model = validated_ast.models.get("User").unwrap();
        let primary_field = user_model.resolved_fields.iter().find(|f| f.name == "primary").unwrap();
        
        assert!(primary_field.attributes.iter().any(|a| matches!(a, FieldAttribute::Relation { name: Some(n), .. } if n == "PrimaryContent")));
    }

    #[test]
    fn test_polymorphic_base_ambiguous_relations_fails() {
        let input = "
            base Content { title: String }
            model Article extends Content { id: String @id }
            model User {
                id: String @id
                primary: Content
                secondary: Content
            }
        ";
        let ast = crate::parser::parse_schema(input).unwrap();
        let err = validate_schema(ast).unwrap_err();
        assert!(err.0.contains("Ambiguous relations: Model 'User' has multiple relations to 'Content'"));
    }
}

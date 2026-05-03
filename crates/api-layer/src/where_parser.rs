use serde_json::Value;
use query_compiler::ir::{WhereClause, WhereCondition, RelationFilter};
use schema_parser::ast::{ModelNode, FieldAttribute, AstFieldType, SchemaAst};

pub fn val_to_string(val: &Value, type_name: Option<&str>) -> Result<String, String> {
    match type_name {
        Some("Float") => {
            if let Some(n) = val.as_f64() {
                Ok(n.to_string())
            } else {
                Err("Validation Error: Expected a Float in filter.".to_string())
            }
        },
        Some("DateTime") => {
            let str_val = val.as_str().ok_or_else(|| "Validation Error: Invalid ISO-8601 DateTime format in filter.".to_string())?;
            let date = chrono::DateTime::parse_from_rfc3339(str_val)
                .map_err(|_| "Validation Error: Invalid ISO-8601 DateTime format in filter.".to_string())?;
            let normalized = date.with_timezone(&chrono::Utc).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            Ok(normalized)
        },
        _ => {
            if let Some(s) = val.as_str() {
                Ok(s.to_string())
            } else if let Some(n) = val.as_number() {
                Ok(n.to_string())
            } else if let Some(b) = val.as_bool() {
                if b { Ok("1".to_string()) } else { Ok("0".to_string()) }
            } else {
                Ok("".to_string())
            }
        }
    }
}

pub fn parse_where_condition(val: &Value, type_name: Option<&str>) -> Result<WhereCondition, String> {
    if val.is_null() {
        return Ok(WhereCondition::IsNull);
    }
    if val.as_str().is_some() || val.as_number().is_some() || val.as_bool().is_some() {
        return Ok(WhereCondition::Eq(val_to_string(val, type_name)?));
    }
    if let Some(obj) = val.as_object() {
        if let Some(eq) = obj.get("eq") {
            if eq.is_null() {
                return Ok(WhereCondition::IsNull);
            }
            return Ok(WhereCondition::Eq(val_to_string(eq, type_name)?));
        }
        if let Some(neq) = obj.get("notEq") {
            if neq.is_null() {
                return Ok(WhereCondition::IsNotNull);
            }
            return Ok(WhereCondition::NotEq(val_to_string(neq, type_name)?));
        }
        if let Some(gt) = obj.get("gt") {
            return Ok(WhereCondition::Gt(val_to_string(gt, type_name)?));
        }
        if let Some(gte) = obj.get("gte") {
            return Ok(WhereCondition::Gte(val_to_string(gte, type_name)?));
        }
        if let Some(lt) = obj.get("lt") {
            return Ok(WhereCondition::Lt(val_to_string(lt, type_name)?));
        }
        if let Some(lte) = obj.get("lte") {
            return Ok(WhereCondition::Lte(val_to_string(lte, type_name)?));
        }
        if let Some(in_vals) = obj.get("in").and_then(|v| v.as_array()) {
            let vals: Result<Vec<String>, String> = in_vals.iter()
                .filter(|v| !v.is_null())
                .map(|v| val_to_string(v, type_name))
                .collect();
            return Ok(WhereCondition::In(vals?));
        }
        if let Some(is_null) = obj.get("isNull") {
            if is_null.as_bool().unwrap_or(false) {
                return Ok(WhereCondition::IsNull);
            } else {
                return Ok(WhereCondition::IsNotNull);
            }
        }
    }
    Err("Invalid where condition format".to_string())
}

pub fn parse_where_clause(ast: &SchemaAst, where_obj: &serde_json::Map<String, Value>, model_def: &ModelNode) -> Result<WhereClause, String> {
    let mut clauses = Vec::new();

    for (k, v) in where_obj {
        if k == "AND" {
            if let Some(arr) = v.as_array() {
                let mut and_clauses = Vec::new();
                for item in arr {
                    if let Some(obj) = item.as_object() {
                        and_clauses.push(parse_where_clause(ast, obj, model_def)?);
                    }
                }
                clauses.push(WhereClause::And(and_clauses));
            }
            continue;
        }
        if k == "OR" {
            if let Some(arr) = v.as_array() {
                let mut or_clauses = Vec::new();
                for item in arr {
                    if let Some(obj) = item.as_object() {
                        or_clauses.push(parse_where_clause(ast, obj, model_def)?);
                    }
                }
                clauses.push(WhereClause::Or(or_clauses));
            }
            continue;
        }

        let field_def = match model_def.resolved_fields.iter().find(|f| &f.name == k) {
            Some(f) => f,
            None => {
                if k.starts_with("__") {
                    // Synthetic markers injected by polymorphism are implicitly booleans
                    let cond = parse_where_condition(v, None)?;
                    clauses.push(WhereClause::Field(k.clone(), cond));
                    continue;
                } else {
                    return Err(format!("Invalid field '{}' in where clause for model '{}'.", k, model_def.name));
                }
            }
        };

        let type_name = match &field_def.field_type {
            AstFieldType::Scalar(t) => Some(t.as_str()),
            AstFieldType::ScalarArray(t) => Some(t.as_str()),
            _ => None,
        };

        match &field_def.field_type {
            AstFieldType::Scalar(_) | AstFieldType::ScalarArray(_) | AstFieldType::PolymorphicUnionArray(_) | AstFieldType::PolymorphicBaseArray(_) => {
                if let Some(obj) = v.as_object() {
                    // If it's an object, it might contain multiple operators like { gte: 20, lt: 30 }
                    // We only treat it as operators if it's NOT a complex relational filter object (some/every/etc)
                    // though for scalars it wouldn't be.
                    let mut operators_found = false;
                    for (op, op_val) in obj {
                        let cond = match op.as_str() {
                            "eq" => if op_val.is_null() { Some(WhereCondition::IsNull) } else { Some(WhereCondition::Eq(val_to_string(op_val, type_name)?)) },
                            "notEq" => if op_val.is_null() { Some(WhereCondition::IsNotNull) } else { Some(WhereCondition::NotEq(val_to_string(op_val, type_name)?)) },
                            "gt" => Some(WhereCondition::Gt(val_to_string(op_val, type_name)?)),
                            "gte" => Some(WhereCondition::Gte(val_to_string(op_val, type_name)?)),
                            "lt" => Some(WhereCondition::Lt(val_to_string(op_val, type_name)?)),
                            "lte" => Some(WhereCondition::Lte(val_to_string(op_val, type_name)?)),
                            "in" => {
                                let vals = op_val.as_array().ok_or("Operator 'in' expects an array")?
                                    .iter().filter(|v| !v.is_null()).map(|v| val_to_string(v, type_name)).collect::<Result<Vec<String>, String>>()?;
                                Some(WhereCondition::In(vals))
                            },
                            "isNull" => if op_val.as_bool().unwrap_or(false) { Some(WhereCondition::IsNull) } else { Some(WhereCondition::IsNotNull) },
                            _ => None
                        };
                        if let Some(c) = cond {
                            clauses.push(WhereClause::Field(k.clone(), c));
                            operators_found = true;
                        }
                    }
                    if !operators_found {
                         let cond = parse_where_condition(v, type_name)?;
                         clauses.push(WhereClause::Field(k.clone(), cond));
                    }
                } else {
                    let cond = parse_where_condition(v, type_name)?;
                    clauses.push(WhereClause::Field(k.clone(), cond));
                }
            }
            AstFieldType::PolymorphicUnion(target_name) | AstFieldType::PolymorphicBase(target_name) => {
                if let Some(obj) = v.as_object() {
                    let is_relational_op = obj.keys().any(|key| ["some", "every", "none", "is", "isNot"].contains(&key.as_str()));
                    
                    if is_relational_op {
                         // Parse Relation Filter for Polymorphic Union/Base
                         // (Logic similar to Relation below but with specific target handling if needed)
                         // For now we only handled 'is' in translator.rs or similar? 
                         // Actually the current code handles 'is' via the next block if it's not a relational op.
                    }

                    if obj.len() == 1 && !is_relational_op {
                        let (specific_target, inner_where) = obj.iter().next().unwrap();
                        
                        let target_model_def = ast.models.get(specific_target)
                            .ok_or_else(|| format!("Undefined target model '{}' in polymorphic filter", specific_target))?;
                            
                        let mut is_valid_target = false;
                        if let Some(union_targets) = ast.unions.get(target_name) {
                            is_valid_target = union_targets.contains(specific_target);
                        } else if let Some(_target_base) = target_model_def.resolved_bases.iter().find(|b| *b == target_name) {
                            is_valid_target = true;
                        }
                        
                        if is_valid_target {
                            let sub_clause = parse_where_clause(ast, inner_where.as_object().unwrap_or(obj), target_model_def)?;
                            
                            clauses.push(WhereClause::Field(format!("{}_type", k), query_compiler::ir::WhereCondition::Eq(specific_target.clone())));
                            
                            clauses.push(WhereClause::Relation {
                                field_name: k.clone(),
                                target_model: specific_target.clone(),
                                fk_column: format!("{}_id", k),
                                is_forward: true,
                                filter: query_compiler::ir::RelationFilter::Is(Box::new(sub_clause)),
                            });
                            continue;
                        }
                    }
                }
                
                let cond = parse_where_condition(v, None)?;
                clauses.push(WhereClause::Field(k.clone(), cond));
            }
            AstFieldType::Relation(target_model_name) | AstFieldType::RelationArray(target_model_name) => {
                let is_relational_op = if let Some(obj) = v.as_object() {
                    obj.keys().any(|key| ["some", "every", "none", "is", "isNot"].contains(&key.as_str()))
                } else {
                    false
                };

                if v.is_null() || !is_relational_op {
                    let cond = parse_where_condition(v, None)?;
                    clauses.push(WhereClause::Field(k.clone(), cond));
                } else if let Some(obj) = v.as_object() {
                    // Parse Relation Filter
                    let target_model_def = ast.models.get(target_model_name)
                        .ok_or_else(|| format!("Undefined target model '{}'", target_model_name))?;

                    let mut we_hold_fk = false;
                    let mut fk_column = "".to_string();

                    for attr in &field_def.attributes {
                        if let FieldAttribute::InternalRelation { fields, .. } = attr {
                            if !fields.is_empty() {
                                if !matches!(field_def.field_type, AstFieldType::RelationArray(_)) && model_def.resolved_fields.iter().any(|f| &f.name == &fields[0]) {
                                    we_hold_fk = true;
                                    fk_column = fields[0].clone();
                                }
                            }
                        }
                    }

                    if !we_hold_fk {
                        // The other side holds the FK
                        let mut found_inverse = false;
                        for target_field in &target_model_def.resolved_fields {
                            if let AstFieldType::Relation(back_target) | AstFieldType::RelationArray(back_target) = &target_field.field_type {
                                if back_target == &model_def.name {
                                    for attr in &target_field.attributes {
                                        if let FieldAttribute::InternalRelation { fields, .. } = attr {
                                            if !fields.is_empty() {
                                                we_hold_fk = false;
                                                fk_column = fields[0].clone();
                                                found_inverse = true;
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                            if found_inverse { break; }
                        }
                        if !found_inverse {
                            return Err(format!("Could not determine foreign key for relation '{}'", k));
                        }
                    }

                    if let Some(some_val) = obj.get("some").and_then(|v| v.as_object()) {
                        let inner = parse_where_clause(ast, some_val, target_model_def)?;
                        clauses.push(WhereClause::Relation {
                            field_name: k.clone(),
                            target_model: target_model_name.clone(),
                            fk_column: fk_column.clone(),
                            is_forward: we_hold_fk,
                            filter: RelationFilter::Some(Box::new(inner)),
                        });
                    }
                    if let Some(every_val) = obj.get("every").and_then(|v| v.as_object()) {
                        let inner = parse_where_clause(ast, every_val, target_model_def)?;
                        clauses.push(WhereClause::Relation {
                            field_name: k.clone(),
                            target_model: target_model_name.clone(),
                            fk_column: fk_column.clone(),
                            is_forward: we_hold_fk,
                            filter: RelationFilter::Every(Box::new(inner)),
                        });
                    }
                    if let Some(none_val) = obj.get("none").and_then(|v| v.as_object()) {
                        let inner = parse_where_clause(ast, none_val, target_model_def)?;
                        clauses.push(WhereClause::Relation {
                            field_name: k.clone(),
                            target_model: target_model_name.clone(),
                            fk_column: fk_column.clone(),
                            is_forward: we_hold_fk,
                            filter: RelationFilter::None(Box::new(inner)),
                        });
                    }
                    if let Some(is_val) = obj.get("is").and_then(|v| v.as_object()) {
                        let inner = parse_where_clause(ast, is_val, target_model_def)?;
                        clauses.push(WhereClause::Relation {
                            field_name: k.clone(),
                            target_model: target_model_name.clone(),
                            fk_column: fk_column.clone(),
                            is_forward: we_hold_fk,
                            filter: RelationFilter::Is(Box::new(inner)),
                        });
                    }
                    if let Some(is_not_val) = obj.get("isNot").and_then(|v| v.as_object()) {
                        let inner = parse_where_clause(ast, is_not_val, target_model_def)?;
                        clauses.push(WhereClause::Relation {
                            field_name: k.clone(),
                            target_model: target_model_name.clone(),
                            fk_column: fk_column.clone(),
                            is_forward: we_hold_fk,
                            filter: RelationFilter::IsNot(Box::new(inner)),
                        });
                    }
                } else {
                    return Err(format!("Expected object for relational filter on '{}'", k));
                }
            }
        }
    }

    if clauses.is_empty() {
        return Ok(WhereClause::AlwaysTrue);
    }

    if clauses.len() == 1 {
        Ok(clauses.remove(0))
    } else {
        Ok(WhereClause::And(clauses))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_val_to_string_float() {
        assert_eq!(val_to_string(&json!(10.5), Some("Float")).unwrap(), "10.5");
        assert_eq!(val_to_string(&json!(10), Some("Float")).unwrap(), "10");
        assert!(val_to_string(&json!("10.5"), Some("Float")).is_err());
    }

    #[test]
    fn test_val_to_string_datetime() {
        assert_eq!(
            val_to_string(&json!("2025-10-10T12:00:00-04:00"), Some("DateTime")).unwrap(),
            "2025-10-10T16:00:00.000Z"
        );
        assert!(val_to_string(&json!("Next Tuesday"), Some("DateTime")).is_err());
    }

    #[test]
    fn test_parse_where_condition_datetime() {
        let condition = parse_where_condition(&json!({ "gte": "2025-10-10T12:00:00-04:00" }), Some("DateTime")).unwrap();
        assert_eq!(condition, WhereCondition::Gte("2025-10-10T16:00:00.000Z".to_string()));

        let err = parse_where_condition(&json!({ "gte": "Next Tuesday" }), Some("DateTime"));
        assert!(err.is_err());
    }

    #[test]
    fn test_val_to_string_fallback() {
        assert_eq!(val_to_string(&json!("Alice"), None).unwrap(), "Alice");
        assert_eq!(val_to_string(&json!(true), None).unwrap(), "1");
        assert_eq!(val_to_string(&json!(null), None).unwrap(), "");
    }
}

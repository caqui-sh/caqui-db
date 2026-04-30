use serde_json::Value;
use query_compiler::ir::{WhereClause, WhereCondition};
use schema_parser::ast::{ModelNode, FieldAttribute};

pub fn val_to_string(val: &Value) -> String {
    if let Some(s) = val.as_str() {
        s.to_string()
    } else if let Some(n) = val.as_number() {
        n.to_string()
    } else if let Some(b) = val.as_bool() {
        b.to_string()
    } else {
        "".to_string()
    }
}

pub fn parse_where_condition(val: &Value) -> Result<WhereCondition, String> {
    if val.is_null() {
        return Ok(WhereCondition::IsNull);
    }
    if let Some(s) = val.as_str() {
        return Ok(WhereCondition::Eq(s.to_string()));
    }
    if let Some(n) = val.as_number() {
        return Ok(WhereCondition::Eq(n.to_string()));
    }
    if let Some(b) = val.as_bool() {
        return Ok(WhereCondition::Eq(if b { "true".to_string() } else { "false".to_string() }));
    }
    if let Some(obj) = val.as_object() {
        if let Some(eq) = obj.get("eq") {
            if eq.is_null() {
                return Ok(WhereCondition::IsNull);
            }
            return Ok(WhereCondition::Eq(val_to_string(eq)));
        }
        if let Some(neq) = obj.get("notEq") {
            if neq.is_null() {
                return Ok(WhereCondition::IsNotNull);
            }
            return Ok(WhereCondition::NotEq(val_to_string(neq)));
        }
        if let Some(gt) = obj.get("gt") {
            return Ok(WhereCondition::Gt(val_to_string(gt)));
        }
        if let Some(gte) = obj.get("gte") {
            return Ok(WhereCondition::Gte(val_to_string(gte)));
        }
        if let Some(lt) = obj.get("lt") {
            return Ok(WhereCondition::Lt(val_to_string(lt)));
        }
        if let Some(lte) = obj.get("lte") {
            return Ok(WhereCondition::Lte(val_to_string(lte)));
        }
        if let Some(in_vals) = obj.get("in").and_then(|v| v.as_array()) {
            let vals: Vec<String> = in_vals.iter()
                .filter(|v| !v.is_null())
                .map(|v| val_to_string(v))
                .collect();
            return Ok(WhereCondition::In(vals));
        }
    }
    Err("Invalid where condition format".to_string())
}

pub fn parse_where_clause(where_obj: &serde_json::Map<String, Value>, model_def: &ModelNode) -> Result<WhereClause, String> {
    let mut clauses = Vec::new();

    for (k, v) in where_obj {
        if k == "AND" {
            if let Some(arr) = v.as_array() {
                let mut and_clauses = Vec::new();
                for item in arr {
                    if let Some(obj) = item.as_object() {
                        and_clauses.push(parse_where_clause(obj, model_def)?);
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
                        or_clauses.push(parse_where_clause(obj, model_def)?);
                    }
                }
                clauses.push(WhereClause::Or(or_clauses));
            }
            continue;
        }

        let field_def = model_def.fields.iter().find(|f| &f.name == k)
            .ok_or_else(|| format!("Invalid field '{}' in where clause for model '{}'.", k, model_def.name))?;

        if field_def.attributes.iter().any(|a| matches!(a, FieldAttribute::Ignore)) {
            return Err(format!("Security Exception: Prohibited filter on ignored field '{}'", k));
        }

        let cond = parse_where_condition(v)?;
        clauses.push(WhereClause::Field(k.clone(), cond));
    }

    if clauses.is_empty() {
        return Err("Empty where clause".to_string());
    }

    if clauses.len() == 1 {
        Ok(clauses.remove(0))
    } else {
        Ok(WhereClause::And(clauses))
    }
}

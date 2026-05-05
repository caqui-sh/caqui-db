use deadpool_sqlite::Pool;
use serde_json::Value;
use std::collections::HashMap;
use query_compiler::mutation_ir::{ExecutionStep, ExecutionPlan, Parameter};

pub fn parse_json_payload(json_payload: &str) -> Result<Value, String> {
    if json_payload.trim().is_empty() {
        return Err("Empty payload".to_string());
    }

    let parsed_data: Value = serde_json::from_str(&json_payload).map_err(|e| e.to_string())?;

    Ok(parsed_data)
}

fn resolve_params(
    params: &[Parameter],
    extra_param: Option<&Parameter>,
    returned_values: &HashMap<String, String>,
) -> Result<Vec<Box<dyn rusqlite::ToSql>>, rusqlite::Error> {
    let mut sql_params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    let iter = params.iter().chain(extra_param.into_iter());
    
    for param in iter {
        match param {
            Parameter::Literal(val) => {
                if let Some(s) = val.as_str() {
                    sql_params.push(Box::new(s.to_string()));
                } else if let Some(n) = val.as_i64() {
                    sql_params.push(Box::new(n));
                } else if let Some(n) = val.as_f64() {
                    sql_params.push(Box::new(n));
                } else if let Some(b) = val.as_bool() {
                    sql_params.push(Box::new(b));
                } else if val.is_null() {
                    sql_params.push(Box::new(rusqlite::types::Null));
                } else {
                    return Err(rusqlite::Error::ToSqlConversionFailure(
                        Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, "Unsupported JSON value for scalar binding"))
                    ));
                }
            },
            Parameter::Reference { step_id, .. } => {
                if let Some(val) = returned_values.get(step_id) {
                    sql_params.push(Box::new(val.clone()));
                } else {
                    return Err(rusqlite::Error::ToSqlConversionFailure(
                        Box::new(std::io::Error::new(std::io::ErrorKind::NotFound, "Missing reference ID"))
                    ));
                }
            }
        }
    }
    Ok(sql_params)
}

fn execute_steps(
    tx: &rusqlite::Transaction,
    steps: &[ExecutionStep],
    returned_values: &mut HashMap<String, String>,
    root_step_id: &str,
    root_id: &mut String,
) -> Result<(), rusqlite::Error> {
    for step in steps {
        match step {
            ExecutionStep::Query { id, sql, params } => {
                let mut stmt = tx.prepare_cached(sql)?;
                let sql_params = resolve_params(params, None, returned_values)?;

                let borrowed_params: Vec<&dyn rusqlite::ToSql> = sql_params.iter().map(|b| &**b).collect();
                let returned_id: String = match stmt.query_row(&borrowed_params[..], |row| {
                    let val: rusqlite::types::Value = row.get(0)?;
                    match val {
                        rusqlite::types::Value::Integer(i) => Ok(i.to_string()),
                        rusqlite::types::Value::Text(s) => Ok(s),
                        rusqlite::types::Value::Real(f) => Ok(f.to_string()),
                        _ => Err(rusqlite::Error::InvalidColumnType(0, "Returned ID is not string or int".to_string(), rusqlite::types::Type::Null)),
                    }
                }) {
                    Ok(id_val) => id_val,
                    Err(rusqlite::Error::QueryReturnedNoRows) => {
                        if id.contains("_disconnect") || id.contains("_set") || id.contains("_cascade") || sql.trim_start().to_uppercase().starts_with("INSERT") {
                            "".to_string()
                        } else {
                            return Err(rusqlite::Error::ToSqlConversionFailure(
                                Box::new(std::io::Error::new(std::io::ErrorKind::NotFound, "Record not found"))
                            ));
                        }
                    }
                    Err(e) => return Err(e),
                };
                
                if id == root_step_id {
                    *root_id = returned_id.clone();
                }
                
                returned_values.insert(id.clone(), returned_id);
            },
            ExecutionStep::UpdateBranch { id, sql, params, parent_ref } => {
                let mut stmt = tx.prepare_cached(sql)?;
                let sql_params = resolve_params(params, Some(parent_ref), returned_values)?;
                let borrowed_params: Vec<&dyn rusqlite::ToSql> = sql_params.iter().map(|b| &**b).collect();
                let returned_id: String = match stmt.query_row(&borrowed_params[..], |row| {
                    let val: rusqlite::types::Value = row.get(0)?;
                    match val {
                        rusqlite::types::Value::Integer(i) => Ok(i.to_string()),
                        rusqlite::types::Value::Text(s) => Ok(s),
                        rusqlite::types::Value::Real(f) => Ok(f.to_string()),
                        _ => Err(rusqlite::Error::InvalidColumnType(0, "Returned ID is not string or int".to_string(), rusqlite::types::Type::Null)),
                    }
                }) {
                    Ok(id_val) => id_val,
                    Err(rusqlite::Error::QueryReturnedNoRows) => {
                        return Err(rusqlite::Error::ToSqlConversionFailure(
                            Box::new(std::io::Error::new(std::io::ErrorKind::NotFound, "Scoped Security Violation or Record Not Found: The targeted record does not exist or does not belong to the parent."))
                        ));
                    }
                    Err(e) => return Err(e),
                };
                if id == root_step_id { *root_id = returned_id.clone(); }
                returned_values.insert(id.clone(), returned_id);
            },
            ExecutionStep::DeleteBranch { id, sql, params, parent_ref } => {
                let mut stmt = tx.prepare_cached(sql)?;
                let sql_params = resolve_params(params, Some(parent_ref), returned_values)?;
                let borrowed_params: Vec<&dyn rusqlite::ToSql> = sql_params.iter().map(|b| &**b).collect();
                let returned_id: String = match stmt.query_row(&borrowed_params[..], |row| {
                    let val: rusqlite::types::Value = row.get(0)?;
                    match val {
                        rusqlite::types::Value::Integer(i) => Ok(i.to_string()),
                        rusqlite::types::Value::Text(s) => Ok(s),
                        rusqlite::types::Value::Real(f) => Ok(f.to_string()),
                        _ => Err(rusqlite::Error::InvalidColumnType(0, "Returned ID is not string or int".to_string(), rusqlite::types::Type::Null)),
                    }
                }) {
                    Ok(id_val) => id_val,
                    Err(rusqlite::Error::QueryReturnedNoRows) => {
                        return Err(rusqlite::Error::ToSqlConversionFailure(
                            Box::new(std::io::Error::new(std::io::ErrorKind::NotFound, "Scoped Security Violation or Record Not Found: The targeted record does not exist or does not belong to the parent."))
                        ));
                    }
                    Err(e) => return Err(e),
                };
                if id == root_step_id { *root_id = returned_id.clone(); }
                returned_values.insert(id.clone(), returned_id);
            },
            ExecutionStep::UpdateMany { id, queries } => {
                let mut total_affected: usize = 0;
                for (sql, params) in queries {
                    println!("DEBUG: UpdateMany SQL: {} with params: {:?}", sql, params);
                    let mut stmt = tx.prepare_cached(sql)?;
                    let sql_params = resolve_params(params, None, returned_values)?;
                    let borrowed_params: Vec<&dyn rusqlite::ToSql> = sql_params.iter().map(|b| &**b).collect();
                    total_affected += stmt.execute(&borrowed_params[..])?;
                }
                let count_str = total_affected.to_string();
                if id == root_step_id { *root_id = count_str.clone(); }
                returned_values.insert(id.clone(), count_str);
            },
            ExecutionStep::DeleteMany { id, queries } => {
                let mut total_affected: usize = 0;
                for (sql, params) in queries {
                    let mut stmt = tx.prepare_cached(sql)?;
                    let sql_params = resolve_params(params, None, returned_values)?;
                    let borrowed_params: Vec<&dyn rusqlite::ToSql> = sql_params.iter().map(|b| &**b).collect();
                    total_affected += stmt.execute(&borrowed_params[..])?;
                }
                let count_str = total_affected.to_string();
                if id == root_step_id { *root_id = count_str.clone(); }
                returned_values.insert(id.clone(), count_str);
            },
        }
    }
    Ok(())
}

pub async fn execute_mutation_plan(pool: &Pool, plan: ExecutionPlan) -> Result<String, String> {
    let conn = pool.get().await.map_err(|e| e.to_string())?;

    let result = conn.interact(move |db| -> Result<String, rusqlite::Error> {
        let tx = db.transaction()?;
        let mut returned_values: HashMap<String, String> = HashMap::new();
        let mut root_id = String::new();

        execute_steps(&tx, &plan.steps, &mut returned_values, &plan.root_step_id, &mut root_id)?;

        tx.commit()?;
        Ok(root_id)
    }).await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use query_compiler::mutation_ir::Parameter;
    use std::collections::HashMap;

    #[test]
    fn test_resolve_params_literal() {
        let params = vec![Parameter::Literal(serde_json::json!("hello")), Parameter::Literal(serde_json::json!(42))];
        let returned = HashMap::new();
        let resolved = resolve_params(&params, None, &returned).unwrap();
        assert_eq!(resolved.len(), 2);
    }

    #[test]
    fn test_resolve_params_reference() {
        let params = vec![Parameter::Reference { step_id: "step_1".to_string(), column: "__id".to_string() }];
        let mut returned = HashMap::new();
        returned.insert("step_1".to_string(), "uuid-123".to_string());
        let resolved = resolve_params(&params, None, &returned).unwrap();
        assert_eq!(resolved.len(), 1);
    }

    #[test]
    fn test_resolve_params_missing_reference() {
        let params = vec![Parameter::Reference { step_id: "step_2".to_string(), column: "__id".to_string() }];
        let returned = HashMap::new();
        let res = resolve_params(&params, None, &returned);
        assert!(res.is_err());
    }

    #[test]
    fn test_resolve_params_with_extra() {
        let params = vec![Parameter::Literal(serde_json::json!("test"))];
        let extra = Parameter::Literal(serde_json::json!(true));
        let returned = HashMap::new();
        let resolved = resolve_params(&params, Some(&extra), &returned).unwrap();
        assert_eq!(resolved.len(), 2);
    }

    #[test]
    fn test_execute_steps_batch_zero_rows_success() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, val TEXT);").unwrap();
        let tx = conn.transaction().unwrap();

        let step = ExecutionStep::UpdateMany {
            id: "step_1".to_string(),
            queries: vec![
                ("UPDATE test SET val = ?1 WHERE id = ?2".to_string(), vec![Parameter::Literal(serde_json::json!("new")), Parameter::Literal(serde_json::json!("non_existent"))])
            ],
        };

        let mut returned = HashMap::new();
        let mut root_id = String::new();
        let res = execute_steps(&tx, &[step], &mut returned, "step_1", &mut root_id);
        
        assert!(res.is_ok());
        assert_eq!(root_id, "0"); // Batch ops return "0" for root ID when no rows are affected
    }

    #[test]
    fn test_execute_steps_batch_multiple_rows_affected() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, val TEXT);").unwrap();
        conn.execute("INSERT INTO test (id, val) VALUES (?1, ?2)", ["1", "old"]).unwrap();
        conn.execute("INSERT INTO test (id, val) VALUES (?1, ?2)", ["2", "old"]).unwrap();
        let tx = conn.transaction().unwrap();

        let step = ExecutionStep::UpdateMany {
            id: "step_1".to_string(),
            queries: vec![
                ("UPDATE test SET val = ?1 WHERE val = ?2".to_string(), vec![Parameter::Literal(serde_json::json!("new")), Parameter::Literal(serde_json::json!("old"))])
            ],
        };

        let mut returned = HashMap::new();
        let mut root_id = String::new();
        let res = execute_steps(&tx, &[step], &mut returned, "step_1", &mut root_id);
        
        assert!(res.is_ok());
        assert_eq!(root_id, "2"); 
    }

    #[test]
    fn test_execute_steps_batch_delete_multiple_rows_affected() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, val TEXT);").unwrap();
        conn.execute("INSERT INTO test (id, val) VALUES (?1, ?2)", ["1", "old"]).unwrap();
        conn.execute("INSERT INTO test (id, val) VALUES (?1, ?2)", ["2", "old"]).unwrap();
        conn.execute("INSERT INTO test (id, val) VALUES (?1, ?2)", ["3", "keep"]).unwrap();
        let tx = conn.transaction().unwrap();

        let step = ExecutionStep::DeleteMany {
            id: "step_delete".to_string(),
            queries: vec![
                ("DELETE FROM test WHERE val = ?1".to_string(), vec![Parameter::Literal(serde_json::json!("old"))])
            ],
        };

        let mut returned = HashMap::new();
        let mut root_id = String::new();
        let res = execute_steps(&tx, &[step], &mut returned, "step_delete", &mut root_id);
        
        assert!(res.is_ok());
        assert_eq!(root_id, "2"); 
    }

    #[test]
    fn test_execute_steps_branch_zero_rows_security_violation() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, val TEXT);").unwrap();
        let tx = conn.transaction().unwrap();

        let step = ExecutionStep::UpdateBranch {
            id: "step_1".to_string(),
            sql: "UPDATE test SET val = ?1 WHERE id = ?2 RETURNING id".to_string(),
            params: vec![Parameter::Literal(serde_json::json!("new"))],
            parent_ref: Parameter::Literal(serde_json::json!("non_existent")),
        };

        let mut returned = HashMap::new();
        let mut root_id = String::new();
        let res = execute_steps(&tx, &[step], &mut returned, "step_1", &mut root_id);
        
        assert!(res.is_err());
        let err = res.unwrap_err();
        assert!(err.to_string().contains("Scoped Security Violation"));
    }
}
